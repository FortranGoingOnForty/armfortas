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
use std::collections::{HashMap, HashSet};
use std::io::Write;

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
    /// Does this local carry runtime array metadata through a descriptor even
    /// though it is not allocatable (for example an assumed-shape dummy)?
    descriptor_arg: bool,
    /// Is this a pass-by-reference parameter? If true, `addr` holds a pointer
    /// to the caller's storage. Reads/writes go through the pointer.
    by_ref: bool,
    /// Character variable kind (fixed-length, deferred, or not character).
    char_kind: CharKind,
    /// Derived type name (for component access resolution). Empty for non-derived.
    derived_type: Option<String>,
    /// For PARAMETER-attributed locals whose initializer const-folds:
    /// the compile-time value to inline at every use. When `Some`,
    /// `Expr::Name` lookups should materialize this constant
    /// directly via `b.const_i32`/`b.const_i64`/etc., instead of
    /// loading through `addr`. Audit MAJOR-4: this lets parameters
    /// avoid wasting a `.data` slot per scope.
    inline_const: Option<ConstScalar>,
    /// Fortran `POINTER` attribute on a scalar local.  When true,
    /// `addr` is an `alloca ptr<ty>` — a pointer slot that holds
    /// the address of the associated target (or null when
    /// unassociated).  Reads/writes go through the slot just like
    /// `by_ref`, but `by_ref` is reserved for dummy arguments that
    /// cannot carry a Fortran `POINTER` attribute's semantics
    /// (reassociation via `=>`, dereference on plain assignment).
    is_pointer: bool,
}

/// Lowering context — tracks locals, loop scopes, and symbol table.
struct LowerCtx<'a> {
    locals: HashMap<String, LocalInfo>,
    loops: Vec<LoopScope>,
    st: &'a SymbolTable,
    /// Module-scoped globals visible by (lowercase module name,
    /// lowercase variable name). Populated by the lower_file
    /// pre-pass over `ProgramUnit::Module` units so any subsequent
    /// function that USE-imports the module can resolve the name
    /// to a `GlobalAddr`. Keying by (module, var) is what lets
    /// install_globals_as_locals filter by the current function's
    /// USE statements, honor ONLY lists, and apply renames.
    globals: &'a HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
    /// Names that a `use mod, only: ...` statement explicitly
    /// excluded. install_globals_as_locals populates this from the
    /// difference between a module's exported globals and the
    /// only-list. Audit MAJOR-1: a reference to a name in this
    /// set must produce a compile error rather than silently
    /// lowering to const_int 0.
    filtered_names: HashSet<String>,
    /// For functions: address of the result variable (for RETURN).
    result_addr: Option<ValueId>,
    /// For functions: the return type.
    result_type: Option<IrType>,
    /// True when this function uses the sret (hidden-output-param) convention
    /// because it returns an allocatable array. Stmt::Return emits `ret void`
    /// instead of loading result_addr. Audit6 BLOCKING-1.
    is_alloc_return: bool,
    /// Names of functions in the compilation unit that return allocatable
    /// arrays (sret convention). Used at call sites to detect when to
    /// pass a temp descriptor as the hidden first arg. Audit6 BLOCKING-1.
    alloc_return_funcs: &'a HashSet<String>,
    /// Per-subroutine optional-parameter bitmap: maps lowercase callee name
    /// to a Vec<bool> (one entry per positional parameter, true = OPTIONAL).
    /// Pre-populated by `collect_optional_params` so call sites can pass
    /// null pointers for absent optional arguments (PRESENT support).
    optional_params: &'a HashMap<String, Vec<bool>>,
    /// Per-subroutine/function descriptor-parameter bitmap: maps lowercase
    /// callee name to a Vec<bool> (one entry per positional parameter,
    /// true = lower this dummy through an ArrayDescriptor).
    descriptor_params: &'a HashMap<String, Vec<bool>>,
    /// Lowercase same-module subprogram name → Module::functions index.
    /// Used so same-compilation-unit calls lower to FuncRef::Internal instead
    /// of pretending to be external references.
    internal_funcs: &'a HashMap<String, u32>,
    /// Lowercase names of functions declared ELEMENTAL in this compilation unit.
    elemental_funcs: &'a HashSet<String>,
    /// Map from Fortran statement label (u64) to the IR basic block that
    /// begins at that label. Pre-populated by `collect_label_blocks` before
    /// lowering so that GOTO can branch forward as well as backward.
    label_blocks: HashMap<u64, BlockId>,
}

impl<'a> LowerCtx<'a> {
    fn new(
        st: &'a SymbolTable,
        globals: &'a HashMap<(String, String), ModuleGlobalInfo>,
        type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
        alloc_return_funcs: &'a HashSet<String>,
        optional_params: &'a HashMap<String, Vec<bool>>,
        descriptor_params: &'a HashMap<String, Vec<bool>>,
        internal_funcs: &'a HashMap<String, u32>,
        elemental_funcs: &'a HashSet<String>,
    ) -> Self {
        Self {
            locals: HashMap::new(),
            loops: Vec::new(),
            st,
            globals,
            type_layouts,
            filtered_names: HashSet::new(),
            result_addr: None,
            result_type: None,
            is_alloc_return: false,
            alloc_return_funcs,
            optional_params,
            descriptor_params,
            internal_funcs,
            elemental_funcs,
            label_blocks: HashMap::new(),
        }
    }

    fn insert_scalar(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
    }

    fn insert_param_by_ref(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, descriptor_arg: false, by_ref: true, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
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
///
/// Two passes:
///   1. Walk every `ProgramUnit::Module`, register its globals into
///      `module.globals` (with const-evaluated initializers where
///      possible) and into a `globals` resolution map keyed by
///      lowercase variable name. This map is what `Expr::Name`
///      lowering consults when a name isn't a local.
///   2. Walk every unit again to lower its functions; module units
///      are skipped on this pass since their globals are already
///      installed.
pub fn lower_file(
    units: &[SpannedUnit],
    st: &SymbolTable,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Module {
    let mut module = Module::new("main".into());
    let mut globals: HashMap<(String, String), ModuleGlobalInfo> = HashMap::new();

    // Pass 1: collect module-level variables.
    for unit in units {
        if let ProgramUnit::Module { name, decls, .. } = &unit.node {
            collect_module_globals(&mut module, &mut globals, name, decls);
        }
    }

    // Pass 1.5: walk every program unit (and its `contains` chain)
    // and collect the names of functions whose result variable is
    // declared `allocatable`. Audit6 BLOCKING-1: these need a hidden
    // first parameter at lowering time so the caller can pass a
    // descriptor address into which the function fills its result.
    let mut alloc_return_funcs: HashSet<String> = HashSet::new();
    for unit in units {
        collect_alloc_return_funcs(&unit.node, &mut alloc_return_funcs);
    }

    // Pass 1.6: collect COMMON block variable types from all program
    // units and emit one global per (block, variable) pair. F77 §5.5:
    // all scopes that reference the same COMMON block must share the
    // same backing memory. Each variable gets its own global so the IR
    // type system sees the right element type; full contiguity (needed
    // for EQUIVALENCE across COMMON boundaries) is deferred.
    // Audit6 BLOCKING-2.
    let mut emitted_common: HashSet<String> = HashSet::new();
    for unit in units {
        collect_and_emit_common_globals(&unit.node, &mut module, &mut emitted_common);
    }

    // Pass 1.7: collect optional-parameter bitmaps for every subroutine/function.
    // Maps lowercase callee name → Vec<bool> (per-position, true = OPTIONAL).
    // Used at call sites to pass null pointers for absent optional arguments
    // so PRESENT() works correctly inside the callee.
    let mut optional_params: HashMap<String, Vec<bool>> = HashMap::new();
    for unit in units {
        collect_optional_params(&unit.node, &mut optional_params);
    }

    let mut descriptor_params: HashMap<String, Vec<bool>> = HashMap::new();
    for unit in units {
        collect_descriptor_params(&unit.node, &mut descriptor_params);
    }

    let mut elemental_funcs: HashSet<String> = HashSet::new();
    for unit in units {
        collect_elemental_funcs(&unit.node, &mut elemental_funcs);
    }

    let mut internal_funcs: HashMap<String, u32> = HashMap::new();
    let mut next_internal_idx: u32 = 0;
    for unit in units {
        collect_internal_func_names(&unit.node, &mut internal_funcs, &mut next_internal_idx);
    }

    // Pass 2: lower each unit. Modules already had their globals
    // installed in pass 1; lower_unit's Module arm is a no-op.
    // Top-level units have no host, so an empty host_uses slice.
    let no_host: Vec<crate::ast::decl::SpannedDecl> = Vec::new();
    let no_host_param_consts: HashMap<String, ConstScalar> = HashMap::new();
    for unit in units {
        lower_unit(
            &mut module,
            unit,
            st,
            &globals,
            type_layouts,
            &no_host,
            &no_host_param_consts,
            None,
            &alloc_return_funcs,
            &optional_params,
            &descriptor_params,
            &internal_funcs,
            &elemental_funcs,
            false,
        );
    }
    module
}

fn collect_internal_func_names(
    unit: &ProgramUnit,
    out: &mut HashMap<String, u32>,
    next_idx: &mut u32,
) {
    match unit {
        ProgramUnit::Program { name, contains, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let body_name = format!("__prog_{}", fname).to_lowercase();
            out.insert(body_name, *next_idx);
            *next_idx += 1;
            for sub in contains {
                collect_internal_func_names(&sub.node, out, next_idx);
            }
        }
        ProgramUnit::Subroutine { name, bind, contains, .. }
        | ProgramUnit::Function { name, bind, contains, .. } => {
            let idx = *next_idx;
            *next_idx += 1;
            out.insert(name.to_lowercase(), idx);
            if let Some(bind) = bind {
                if let Some(bind_name) = bind.name.as_deref() {
                    out.entry(bind_name.trim_matches('\'').trim_matches('"').to_lowercase())
                        .or_insert(idx);
                }
            }
            for sub in contains {
                collect_internal_func_names(&sub.node, out, next_idx);
            }
        }
        ProgramUnit::Module { contains, .. } => {
            for sub in contains {
                collect_internal_func_names(&sub.node, out, next_idx);
            }
        }
        _ => {}
    }
}

/// Walk a program unit and any nested `contains` to collect the
/// names of functions whose result variable is declared
/// `allocatable`. The set is keyed by lowercase function name and
/// is consulted at call sites in pass 2.
///
/// Audit6 BLOCKING-1: a function `function f() result(r); integer,
/// allocatable :: r(:)` cannot be returned by value through the
/// usual scalar result alloca — the descriptor is 384 bytes and
/// the type system needs to know about it at every call site.
/// We model this with a hidden first `ptr<[i8 x 384]>` parameter
/// that the caller passes in, and the function writes its result
/// into that descriptor.
fn collect_alloc_return_funcs(unit: &ProgramUnit, out: &mut HashSet<String>) {
    use crate::ast::decl::Attribute;
    let scan_decls = |decls: &[crate::ast::decl::SpannedDecl], result_name: &str| -> bool {
        let key = result_name.to_lowercase();
        for decl in decls {
            if let Decl::TypeDecl { entities, attrs, .. } = &decl.node {
                for entity in entities {
                    if entity.name.to_lowercase() == key {
                        return attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
                    }
                }
            }
        }
        false
    };
    match unit {
        ProgramUnit::Function { name, decls, contains, result, .. } => {
            let result_name = result.as_deref().unwrap_or(name.as_str());
            if scan_decls(decls, result_name) {
                out.insert(name.to_lowercase());
            }
            for sub in contains {
                collect_alloc_return_funcs(&sub.node, out);
            }
        }
        ProgramUnit::Program { contains, .. }
        | ProgramUnit::Subroutine { contains, .. }
        | ProgramUnit::Module { contains, .. } => {
            for sub in contains {
                collect_alloc_return_funcs(&sub.node, out);
            }
        }
        _ => {}
    }
}

/// Scan a program unit and its CONTAINS chain and record, for each
/// subroutine/function, which of its positional dummy arguments carry
/// the OPTIONAL attribute.
///
/// Result: `out` maps lowercase subroutine/function name →
/// `Vec<bool>` (index = parameter position, value = is_optional).
/// Used at call sites to pass null pointers for absent optional args,
/// enabling PRESENT() intrinsic queries inside the callee.
fn collect_optional_params(unit: &ProgramUnit, out: &mut HashMap<String, Vec<bool>>) {
    use crate::ast::decl::Attribute;
    use crate::ast::unit::DummyArg;
    let record = |name: &str, args: &[DummyArg], decls: &[crate::ast::decl::SpannedDecl],
                  out: &mut HashMap<String, Vec<bool>>| {
        let param_names: Vec<String> = args.iter().filter_map(|a| {
            if let DummyArg::Name(n) = a { Some(n.to_lowercase()) } else { None }
        }).collect();
        if param_names.is_empty() { return; }
        let optional_flags: Vec<bool> = param_names.iter().map(|pname| {
            for d in decls {
                if let crate::ast::decl::Decl::TypeDecl { attrs, entities, .. } = &d.node {
                    let is_optional = attrs.iter().any(|a| matches!(a, Attribute::Optional));
                    if is_optional && entities.iter().any(|e| e.name.to_lowercase() == *pname) {
                        return true;
                    }
                }
            }
            false
        }).collect();
        out.insert(name.to_lowercase(), optional_flags);
    };
    match unit {
        ProgramUnit::Subroutine { name, args, decls, contains, .. } => {
            record(name, args, decls, out);
            for sub in contains { collect_optional_params(&sub.node, out); }
        }
        ProgramUnit::Function { name, args, decls, contains, .. } => {
            record(name, args, decls, out);
            for sub in contains { collect_optional_params(&sub.node, out); }
        }
        ProgramUnit::Program { contains, .. } | ProgramUnit::Module { contains, .. } => {
            for sub in contains { collect_optional_params(&sub.node, out); }
        }
        _ => {}
    }
}

fn arg_uses_descriptor_from_decls(
    arg_name: &str,
    decls: &[crate::ast::decl::SpannedDecl],
) -> bool {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let crate::ast::decl::Attribute::Dimension(specs) = a {
                    Some(specs)
                } else {
                    None
                }
            });
            for entity in entities {
                if entity.name.to_lowercase() != key {
                    continue;
                }
                let Some(specs) = entity.array_spec.as_ref().or(attr_dims) else {
                    return false;
                };
                return specs.iter().any(|spec| {
                    matches!(
                        spec,
                        ArraySpec::AssumedShape { .. }
                            | ArraySpec::AssumedSize { .. }
                            | ArraySpec::Deferred
                            | ArraySpec::AssumedRank
                    )
                });
            }
        }
    }
    false
}

/// Record which positional dummy arguments are lowered through an
/// ArrayDescriptor rather than a raw element pointer.
fn collect_descriptor_params(unit: &ProgramUnit, out: &mut HashMap<String, Vec<bool>>) {
    use crate::ast::unit::DummyArg;
    let record = |name: &str, args: &[DummyArg], decls: &[crate::ast::decl::SpannedDecl],
                  out: &mut HashMap<String, Vec<bool>>| {
        let param_names: Vec<String> = args.iter().filter_map(|a| {
            if let DummyArg::Name(n) = a { Some(n.to_lowercase()) } else { None }
        }).collect();
        if param_names.is_empty() {
            return;
        }
        let flags: Vec<bool> = param_names
            .iter()
            .map(|pname| arg_uses_descriptor_from_decls(pname, decls))
            .collect();
        out.insert(name.to_lowercase(), flags);
    };
    match unit {
        ProgramUnit::Subroutine { name, args, decls, contains, .. } => {
            record(name, args, decls, out);
            for sub in contains { collect_descriptor_params(&sub.node, out); }
        }
        ProgramUnit::Function { name, args, decls, contains, .. } => {
            record(name, args, decls, out);
            for sub in contains { collect_descriptor_params(&sub.node, out); }
        }
        ProgramUnit::Program { contains, .. } | ProgramUnit::Module { contains, .. } => {
            for sub in contains { collect_descriptor_params(&sub.node, out); }
        }
        _ => {}
    }
}

/// Collect lowercase names of functions declared ELEMENTAL. Whole-array
/// lowering uses this side table to recognize elemental calls before symbol
/// resolution has become IR call refs.
fn collect_elemental_funcs(unit: &ProgramUnit, out: &mut HashSet<String>) {
    use crate::ast::unit::Prefix;
    match unit {
        ProgramUnit::Function { name, prefix, contains, .. } => {
            if prefix.iter().any(|p| matches!(p, Prefix::Elemental)) {
                out.insert(name.to_lowercase());
            }
            for sub in contains {
                collect_elemental_funcs(&sub.node, out);
            }
        }
        ProgramUnit::Program { contains, .. }
        | ProgramUnit::Subroutine { contains, .. }
        | ProgramUnit::Module { contains, .. } => {
            for sub in contains {
                collect_elemental_funcs(&sub.node, out);
            }
        }
        _ => {}
    }
}

/// Scan a program unit (and its `contains` chain) for `Decl::CommonBlock`
/// statements and emit one scalar global per *slot position* within each
/// COMMON block. All scopes that declare the same block share these
/// globals, giving correct F77 §5.5 shared-memory semantics for scalars.
///
/// Naming: `afs_common_<block_name>_<slot_index>` (lowercase).
/// The blank COMMON uses the synthetic block name `__blank__`.  Using
/// the slot position — not the local variable name — as the disambiguator
/// matters when a contained subprogram aliases the same block under
/// different local names: `common /blk/ a, b` in the host and
/// `common /blk/ x, y` in a contained routine must resolve to the same
/// two globals, not four separate ones.  The slot's element type comes
/// from whichever scope the module walker visits first; scopes that
/// agree on positional types will get correct reads/writes, and
/// scopes that disagree are an F77 undefined-behavior region we
/// leave unhandled for now.  Audit6 BLOCKING-2.
fn common_slot_symbol(block: &str, slot_idx: usize) -> String {
    format!("afs_common_{}_{}", block, slot_idx)
}

fn collect_and_emit_common_globals(
    unit: &ProgramUnit,
    module: &mut Module,
    emitted: &mut HashSet<String>,
) {
    use crate::ast::decl::Decl;
    let emit_for_decls = |decls: &[crate::ast::decl::SpannedDecl], module: &mut Module, emitted: &mut HashSet<String>| {
        for decl in decls {
            if let Decl::CommonBlock { name, vars } = &decl.node {
                let block_name = name.as_deref().unwrap_or("__blank__").to_lowercase();
                for (slot_idx, var) in vars.iter().enumerate() {
                    let symbol = common_slot_symbol(&block_name, slot_idx);
                    if emitted.contains(&symbol) { continue; }
                    emitted.insert(symbol.clone());
                    let elem_ty = arg_type_from_decls(&var.to_lowercase(), decls);
                    module.add_global(Global {
                        name: symbol,
                        ty: elem_ty,
                        initializer: Some(GlobalInit::Zero),
                    });
                }
            }
        }
    };
    match unit {
        ProgramUnit::Program { decls, contains, .. } => {
            emit_for_decls(decls, module, emitted);
            for sub in contains { collect_and_emit_common_globals(&sub.node, module, emitted); }
        }
        ProgramUnit::Subroutine { decls, contains, .. } => {
            emit_for_decls(decls, module, emitted);
            for sub in contains { collect_and_emit_common_globals(&sub.node, module, emitted); }
        }
        ProgramUnit::Function { decls, contains, .. } => {
            emit_for_decls(decls, module, emitted);
            for sub in contains { collect_and_emit_common_globals(&sub.node, module, emitted); }
        }
        _ => {}
    }
}

/// Install COMMON block variables as global_addr locals before `alloc_decls`
/// runs. Because `alloc_decls` skips names already in `locals`, the COMMON
/// variables are not re-alloca'd with private storage. Each variable is
/// installed as a direct (non-by_ref) local whose addr is a GlobalAddr
/// pointing to the shared COMMON global. Audit6 BLOCKING-2.
fn install_common_locals(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
) {
    use crate::ast::decl::Decl;
    for decl in decls {
        if let Decl::CommonBlock { name, vars } = &decl.node {
            let block_name = name.as_deref().unwrap_or("__blank__").to_lowercase();
            for (slot_idx, var) in vars.iter().enumerate() {
                let key = var.to_lowercase();
                if locals.contains_key(&key) { continue; }
                let symbol = common_slot_symbol(&block_name, slot_idx);
                let elem_ty = arg_type_from_decls(&key, decls);
                let addr = b.global_addr(&symbol, elem_ty.clone());
                locals.insert(key, LocalInfo {
                    addr,
                    ty: elem_ty,
                    dims: vec![],
                    allocatable: false,
                    descriptor_arg: false,
                    by_ref: false,
                    char_kind: CharKind::None,
                    derived_type: None,
                    inline_const: None, is_pointer: false,
                });
            }
        }
    }
}

/// Install EQUIVALENCE group members as aliased locals before `alloc_decls`.
///
/// F77 §5.4: each member of an equivalence group must share the same
/// backing storage. We allocate one `[i8 x total]` backing store and
/// install each variable with a GEP into it at its byte offset. The
/// GEP element type matches the variable's declared type so that
/// subsequent loads and stores are correctly typed (the verifier
/// allows `store T, Ptr<T>` unconditionally). Audit6 BLOCKING-3.
///
/// Supported members:
///   * `Expr::Name` — scalar variable at offset 0 within itself.
///   * `Expr::FunctionCall { callee: name, args: [Element(idx)] }` —
///     array element `name(idx)`, at byte offset `(idx−1)*elem_size`
///     relative to the start of the array. Array must already be in
///     scope via a static (non-allocatable) TypeDecl so we can compute
///     the size at compile time.
///
/// The "anchor" of the group is the member with the smallest byte
/// offset after mapping each member's internal offset to a shared
/// coordinate space. All other members are GEP'd at their relative
/// distance from the anchor. The backing store is sized to cover the
/// maximum extent across all members.
fn install_equivalence_locals(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
) {
    use crate::ast::decl::Decl;
    use crate::ast::expr::Expr;
    use crate::ast::expr::SectionSubscript;

    for decl in decls {
        if let Decl::EquivalenceStmt { groups } = &decl.node {
            for group in groups {
                // Resolve each member to (var_name, elem_ty, within_var_byte_offset).
                // within_var_byte_offset: for `name` → 0; for `name(i)` → (i-1)*elem_size.
                let mut members: Vec<(String, IrType, i64)> = Vec::new();
                for expr in group {
                    match &expr.node {
                        Expr::Name { name } => {
                            let key = name.to_lowercase();
                            let ty = arg_type_from_decls(&key, decls);
                            members.push((key, ty, 0));
                        }
                        Expr::FunctionCall { callee, args } => {
                            if let Expr::Name { name } = &callee.node {
                                let key = name.to_lowercase();
                                let ty = arg_type_from_decls(&key, decls);
                                let idx = if let Some(sub) = args.first() {
                                    if let SectionSubscript::Element(e) = &sub.value {
                                        eval_const_int(e).unwrap_or(1)
                                    } else { 1 }
                                } else { 1 };
                                let byte_off = (idx.max(1) - 1) * ir_scalar_byte_size(&ty);
                                members.push((key, ty, byte_off));
                            }
                        }
                        _ => {} // skip complex expressions
                    }
                }
                if members.is_empty() { continue; }

                // Find the smallest within_var offset — this becomes the "origin".
                let min_off = members.iter().map(|(_, _, o)| *o).min().unwrap_or(0);

                // Compute total backing store size (bytes).
                let total = members.iter().map(|(_, ty, o)| {
                    (o - min_off) + ir_scalar_byte_size(ty)
                }).max().unwrap_or(8);

                // Allocate the byte-array backing store.
                let backing_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), total as u64);
                let backing = b.alloca(backing_ty);

                for (var_name, elem_ty, within_off) in &members {
                    if locals.contains_key(var_name) { continue; }
                    let rel = within_off - min_off; // byte offset into backing
                    // GEP with element type = elem_ty so the result is Ptr<elem_ty>.
                    // For I32 at rel=0: gep(backing, [0], I32) → Ptr<I32> (backing itself).
                    // For I32 at rel=4: gep(backing, [1], I32) → Ptr<I32> (backing + 4).
                    let elem_size = ir_scalar_byte_size(elem_ty);
                    let gep_idx = if elem_size > 0 { rel / elem_size } else { 0 };
                    let idx_val = b.const_i64(gep_idx);
                    let addr = b.gep(backing, vec![idx_val], elem_ty.clone());
                    locals.insert(var_name.clone(), LocalInfo {
                        addr,
                        ty: elem_ty.clone(),
                        dims: vec![],
                        allocatable: false,
                        descriptor_arg: false,
                        by_ref: false,
                        char_kind: CharKind::None,
                        derived_type: None,
                        inline_const: None, is_pointer: false,
                    });
                }
            }
        }
    }
}

/// Information about a module-level global, tracked in
/// `lower_file`'s globals map so `install_globals_as_locals` can
/// reconstruct a `LocalInfo` for it inside each function that
/// USE-imports the module.
#[derive(Clone)]
struct ModuleGlobalInfo {
    /// Mach-O symbol name (already prefixed with `afs_mod_<mod>_`).
    symbol: String,
    /// Element type (for scalars) or array element type (for arrays).
    ty: IrType,
    /// Per-dimension `(lower_bound, extent)` pairs. Empty for scalars
    /// and for deferred-shape allocatables.
    dims: Vec<(i64, i64)>,
    /// True for module-level allocatable arrays. The global is a
    /// 384-byte zero-init descriptor; runtime allocate() populates
    /// it. install_globals_as_locals threads this through into
    /// LocalInfo.allocatable so subscript access goes through the
    /// runtime descriptor path. Audit MAJOR-5.
    allocatable: bool,
}

/// Walk a module's declarations and emit a global per variable.
/// Handles scalars (with literal initializers) and fixed-size
/// arrays (with array-constructor initializers). Resolves the
/// initializer at compile time where possible; otherwise falls
/// through to zero-init.
///
/// Array variables with non-literal initializers or dynamic dims
/// are currently rejected by falling through to scalar emission —
/// that's a known gap tracked for follow-up.
fn collect_module_globals(
    module: &mut Module,
    globals: &mut HashMap<(String, String), ModuleGlobalInfo>,
    mod_name: &str,
    decls: &[crate::ast::decl::SpannedDecl],
) {
    use crate::ast::decl::Attribute;
    // Module-level parameter table built incrementally so a later
    // parameter declaration can reference earlier ones.
    let param_consts = collect_decl_param_consts(decls);
    for decl in decls {
        if let Decl::TypeDecl { type_spec, attrs, entities } = &decl.node {
            let ir_ty = lower_type_spec(type_spec);
            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let Attribute::Dimension(specs) = a { Some(specs) } else { None }
            });
            let is_allocatable = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            for entity in entities {
                let symbol = format!("afs_mod_{}_{}",
                    mod_name.to_lowercase(),
                    entity.name.to_lowercase());

                let array_spec = entity.array_spec.as_ref().or(attr_dims);

                // Audit MAJOR-5: module-level allocatable arrays.
                // Emit a 384-byte zero-init descriptor as the
                // global; runtime allocate() populates it. The
                // shape (deferred or fixed) doesn't matter for
                // emission — the descriptor stores it at runtime.
                if is_allocatable && array_spec.is_some() {
                    let desc_ty = IrType::Array(
                        Box::new(IrType::Int(IntWidth::I8)),
                        384,
                    );
                    module.add_global(Global {
                        name: symbol.clone(),
                        ty: desc_ty,
                        initializer: Some(GlobalInit::Zero),
                    });
                    globals.insert(
                        (mod_name.to_lowercase(), entity.name.to_lowercase()),
                        ModuleGlobalInfo {
                            symbol,
                            ty: ir_ty.clone(),
                            dims: vec![],
                            allocatable: true,
                        },
                    );
                    continue;
                }

                if let Some(specs) = array_spec {
                    // Array module variable. Compute dims and
                    // build an array-typed global with a matching
                    // IntArray/FloatArray initializer when the
                    // entity.init is an array constructor of
                    // literal values.
                    let dims = extract_array_dims(specs, &param_consts);
                    let total: i64 = dims.iter().map(|(_, e)| *e).product();
                    if total <= 0 {
                        continue; // assumed/deferred shape — skip
                    }
                    let global_ty = IrType::Array(
                        Box::new(ir_ty.clone()),
                        total as u64,
                    );

                    // Audit MAJOR-3: detect over-long initializer
                    // BEFORE eval_const_array_init returns None.
                    // Per F2018 §7.4.4, the initializer's shape
                    // must conform with the variable's declared
                    // shape; over-long is a hard error.
                    if let Some(init_e) = &entity.init {
                        if let Some(scalars) =
                            collect_const_array_scalars(init_e, &ir_ty, &param_consts)
                        {
                            if (scalars.len() as i64) > total {
                                eprintln!(
                                    "armfortas: error: {}:{}: initializer for '{}' has \
                                     {} elements but its declared shape requires \
                                     {} (audit MAJOR-3 — initializer shape \
                                     mismatch)",
                                    init_e.span.start.line,
                                    init_e.span.start.col,
                                    entity.name,
                                    scalars.len(),
                                    total,
                                );
                                let _ = std::io::stderr().flush();
                                std::process::exit(1);
                            }
                        }
                    }

                    let init = entity.init.as_ref()
                        .and_then(|e| eval_const_array_init(e, &ir_ty, total, &param_consts));
                    module.add_global(Global {
                        name: symbol.clone(),
                        ty: global_ty,
                        initializer: Some(init.unwrap_or(GlobalInit::Zero)),
                    });
                    globals.insert(
                        (mod_name.to_lowercase(), entity.name.to_lowercase()),
                        ModuleGlobalInfo {
                            symbol,
                            ty: ir_ty.clone(),
                            dims,
                            allocatable: false,
                        },
                    );
                } else {
                    // Scalar module variable.
                    let init = entity.init.as_ref()
                        .and_then(|e| eval_const_global_init(e, &param_consts, Some(&ir_ty)));
                    module.add_global(Global {
                        name: symbol.clone(),
                        ty: ir_ty.clone(),
                        initializer: Some(init.unwrap_or(GlobalInit::Zero)),
                    });
                    globals.insert(
                        (mod_name.to_lowercase(), entity.name.to_lowercase()),
                        ModuleGlobalInfo {
                            symbol,
                            ty: ir_ty.clone(),
                            dims: vec![],
                            allocatable: false,
                        },
                    );
                }
            }
        }
    }
}

/// Try to evaluate an array constructor as a `GlobalInit`.
/// Handles three forms:
///   1. `[v0, v1, v2]` literal-element constructor
///   2. `[(expr, i = lo, hi[, step])]` implied-do iterator
///   3. `reshape(constructor, shape)` reshape of (1) or (2)
///
/// Each path produces a flat list of `i128` (for integer types)
/// or `f64` (for float types) of length `total`. Shorter lists
/// are zero-padded; longer lists return `None` (a future Maj-3
/// fix will add a proper diagnostic for shape-mismatch errors).
///
/// Audit MAJOR-2.
fn eval_const_array_init(
    expr: &crate::ast::expr::SpannedExpr,
    elem_ty: &IrType,
    total: i64,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<GlobalInit> {
    let scalars = collect_const_array_scalars(expr, elem_ty, param_consts)?;
    if (scalars.len() as i64) > total {
        // Shape mismatch — too many elements. Return None so the
        // caller falls back to zero-init. A proper diagnostic is
        // tracked under audit MAJOR-3.
        return None;
    }

    let is_float = matches!(elem_ty, IrType::Float(_));
    if is_float {
        let mut out: Vec<f64> = scalars.iter().map(|s| s.to_float()).collect();
        while (out.len() as i64) < total { out.push(0.0); }
        Some(GlobalInit::FloatArray(out))
    } else {
        let mut out: Vec<i128> = scalars.iter().map(|s| match s {
            ConstScalar::Int(i) => *i,
            ConstScalar::Float(f) => *f as i128,
        }).collect();
        while (out.len() as i64) < total { out.push(0); }
        Some(GlobalInit::IntArray(out))
    }
}

/// Recursively collect the scalar elements of a constructor
/// expression into a flat Vec. Used by eval_const_array_init to
/// support nested implied-do, reshape, and parameter references
/// uniformly.
///
/// reshape(source, shape) just produces source's elements in
/// declared order — Fortran's reshape is column-major and
/// reorders dimensions, but for the FLAT linearization the
/// element ordering is identical to source's. We don't yet
/// honor non-trivial shape arguments (only reshape passes that
/// match the source length get folded).
fn collect_const_array_scalars(
    expr: &crate::ast::expr::SpannedExpr,
    elem_ty: &IrType,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<Vec<ConstScalar>> {
    match &expr.node {
        Expr::ArrayConstructor { values, .. } => {
            let mut out: Vec<ConstScalar> = Vec::new();
            for v in values {
                collect_ac_value(v, elem_ty, param_consts, &mut out)?;
            }
            Some(out)
        }
        // reshape(source, shape) — pass through source elements.
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                if name.eq_ignore_ascii_case("reshape") && !args.is_empty() {
                    if let crate::ast::expr::SectionSubscript::Element(src) = &args[0].value {
                        return collect_const_array_scalars(src, elem_ty, param_consts);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Collect a single AcValue (which may be a literal element or
/// an implied-do iterator) into a flat scalar list.
fn collect_ac_value(
    v: &crate::ast::expr::AcValue,
    elem_ty: &IrType,
    param_consts: &HashMap<String, ConstScalar>,
    out: &mut Vec<ConstScalar>,
) -> Option<()> {
    use crate::ast::expr::AcValue;
    match v {
        AcValue::Expr(e) => {
            let raw = eval_const_scalar(e, param_consts)?;
            // Coerce int → float when the destination is float.
            let coerced = if matches!(elem_ty, IrType::Float(_)) {
                ConstScalar::Float(raw.to_float())
            } else {
                raw
            };
            out.push(coerced);
            Some(())
        }
        AcValue::ImpliedDo(ido) => {
            let (values, var, start, end, step) = (&ido.values, &ido.var, &ido.start, &ido.end, &ido.step);
            let start_v = eval_const_scalar(start, param_consts)?;
            let end_v = eval_const_scalar(end, param_consts)?;
            let step_v = match step {
                Some(e) => eval_const_scalar(e, param_consts)?,
                None => ConstScalar::Int(1),
            };
            let (ConstScalar::Int(s), ConstScalar::Int(e), ConstScalar::Int(stp)) =
                (start_v, end_v, step_v) else { return None; };
            if stp == 0 { return None; }

            // Walk the range, evaluating the inner values for each
            // iteration with `var` bound in a temporary param_consts
            // overlay.
            let mut local_consts = param_consts.clone();
            let var_key = var.to_lowercase();
            let mut i = s;
            // Cap iterations to avoid runaway folding for runtime
            // bounds disguised as constants.
            let mut steps_remaining: i64 = 1_000_000;
            let going_down = stp < 0;
            loop {
                if steps_remaining == 0 { return None; }
                steps_remaining -= 1;
                let in_range = if going_down { i >= e } else { i <= e };
                if !in_range { break; }
                local_consts.insert(var_key.clone(), ConstScalar::Int(i));
                for inner in values {
                    collect_ac_value(inner, elem_ty, &local_consts, out)?;
                }
                i = i.wrapping_add(stp);
            }
            Some(())
        }
    }
}

fn lower_unit(
    module: &mut Module,
    unit: &SpannedUnit,
    st: &SymbolTable,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    // Audit CRITICAL-4: USE imports from the host program unit
    // (and its hosts, transitively). Per F2018 §16.2, names
    // imported into a host are visible in its contained
    // subprograms via host association. Each lower_unit call
    // accumulates its own uses on top of host_uses and passes
    // the combined list down to any nested subprogram. The
    // top-level call from lower_file passes an empty slice.
    host_uses: &[crate::ast::decl::SpannedDecl],
    host_param_consts: &HashMap<String, ConstScalar>,
    host_module: Option<&str>,
    alloc_return_funcs: &HashSet<String>,
    optional_params: &HashMap<String, Vec<bool>>,
    descriptor_params: &HashMap<String, Vec<bool>>,
    internal_funcs: &HashMap<String, u32>,
    elemental_funcs: &HashSet<String>,
    internal_only: bool,
) {
    match &unit.node {
        ProgramUnit::Program { name, decls, body, contains, uses, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let visible_param_consts = collect_decl_param_consts_with_host(decls, host_param_consts);
            // Fortran PROGRAM bodies are never the C entry point — driver/mod.rs
            // always emits a `_main` wrapper. Use a private name so a user-written
            // "PROGRAM MAIN" (or unnamed program) never produces a duplicate _main.
            let body_fname = format!("__prog_{}", fname);
            let mut func = Function::new(body_fname.clone(), vec![], IrType::Void);
            let mut ctx = LowerCtx::new(st, globals, type_layouts, alloc_return_funcs, optional_params, descriptor_params, internal_funcs, elemental_funcs);
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();

            // Combined USE list for this unit: host_uses inherited
            // from the program ancestry + this unit's own uses.
            // Programs themselves have no host, so host_uses is
            // typically empty here, but a Program declared inside
            // a Module (rare but legal) would inherit module uses.
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();

            {
                let mut b = FuncBuilder::new(&mut func);
                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(&mut b, &mut ctx.locals, decls);
                alloc_decls(&mut b, &mut ctx.locals, decls, &visible_param_consts, type_layouts, &mut pending_globals, &fname);
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    host_module,
                    ctx.st,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses);
                check_no_filtered_refs(body, &ctx.filtered_names);
                init_decls(&mut b, &ctx.locals, decls, st);
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts, None);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower CONTAINS subprograms with this unit's combined
            // uses as their host_uses, so host association threads
            // through Program → contained Subroutine/Function.
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    true,
                );
            }
        }
        ProgramUnit::Subroutine { name, decls, body, args, bind, uses, contains, prefix, .. } => {
            // BIND(C): use specified C name, otherwise use Fortran name.
            let func_name = bind.as_ref()
                .map(|b| b.name.as_deref().unwrap_or(name).trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_else(|| name.clone());
            let visible_param_consts = collect_decl_param_consts_with_host(decls, host_param_consts);
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    let elem_ty = arg_type_from_decls(n, decls);
                    let fortran_noalias = arg_is_fortran_noalias(n, decls);
                    let uses_descriptor = arg_uses_descriptor_from_decls(n, decls);
                    if arg_has_value_attr(n, decls) {
                        // VALUE: pass by value (raw type, not pointer).
                        Some(Param {
                            name: n.clone(),
                            ty: elem_ty,
                            id: ValueId(i as u32),
                            fortran_noalias: false,
                        })
                    } else {
                        Some(Param {
                            name: n.clone(),
                            ty: if uses_descriptor {
                                IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)))
                            } else {
                                IrType::Ptr(Box::new(elem_ty))
                            },
                            id: ValueId(i as u32),
                            fortran_noalias,
                        })
                    }
                } else { None }
            }).collect();
            let mut func = Function::new(func_name.clone(), params, IrType::Void);
            use crate::ast::unit::Prefix;
            func.is_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            func.is_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            func.internal_only = internal_only;
            let mut ctx = LowerCtx::new(st, globals, type_layouts, alloc_return_funcs, optional_params, descriptor_params, internal_funcs, elemental_funcs);
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();

            // Collect param info: (name, param_id, elem_type, is_value).
            let param_info: Vec<(String, ValueId, IrType, bool)> = func.params.iter()
                .map(|p| {
                    let pname = p.name.to_lowercase();
                    let elem_ty = arg_type_from_decls(&pname, decls);
                    let is_value = arg_has_value_attr(&pname, decls);
                    (pname, p.id, elem_ty, is_value)
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
                        let uses_descriptor = arg_uses_descriptor_from_decls(pname, decls);
                        let slot = if uses_descriptor {
                            b.alloca(IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384))))
                        } else {
                            b.alloca(IrType::Ptr(Box::new(elem_ty.clone())))
                        };
                        b.store(*pid, slot);
                        // Check if this is a derived type parameter.
                        let dt_name = arg_derived_type_name(pname, decls);
                        let ck = arg_char_kind_from_decls(pname, decls);
                        let info = LocalInfo {
                            addr: slot, ty: elem_ty.clone(),
                            dims: arg_dims_from_decls(pname, decls, &visible_param_consts), allocatable: false, descriptor_arg: uses_descriptor, by_ref: true,
                            char_kind: ck, derived_type: dt_name, inline_const: None, is_pointer: false,
                        };
                        ctx.locals.insert(pname.clone(), info);
                    }
                }

                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(&mut b, &mut ctx.locals, decls);
                alloc_decls(&mut b, &mut ctx.locals, decls, &visible_param_consts, type_layouts, &mut pending_globals, &func_name);
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    host_module,
                    ctx.st,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses);
                check_no_filtered_refs(body, &ctx.filtered_names);
                init_decls(&mut b, &ctx.locals, decls, st);
                // Pre-create blocks for all statement labels so GOTO can branch forward.
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts, None);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower nested CONTAINS subprograms (this was a latent
            // bug — the previous code only walked Program::contains).
            // Each nested sub inherits this subroutine's combined
            // host_uses + own uses.
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    true,
                );
            }
        }
        ProgramUnit::Function { name, decls, body, args, result, return_type, bind, uses, contains, prefix, .. } => {
            let func_name = bind.as_ref()
                .map(|b| b.name.as_deref().unwrap_or(name).trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_else(|| name.clone());
            let visible_param_consts = collect_decl_param_consts_with_host(decls, host_param_consts);

            // Audit6 BLOCKING-1: functions with allocatable result use the
            // sret (hidden-output-param) convention. The caller allocas a
            // 384-byte descriptor and passes its address as param 0; the
            // function writes its result into that descriptor and returns
            // void. This avoids trying to return 384 bytes "by value".
            let is_alloc_return = alloc_return_funcs.contains(&func_name.to_lowercase());

            let (func_params, ir_ret_ty) = if is_alloc_return {
                // Hidden first param: ptr to caller-provided 384-byte descriptor.
                let desc_ptr_ty = IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)));
                let sret = Param {
                    name: "_sret".into(),
                    ty: desc_ptr_ty,
                    id: ValueId(0),
                    fortran_noalias: false,
                };
                // Real args shifted by 1 so _sret is param 0.
                let real: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                    if let DummyArg::Name(n) = arg {
                        let elem_ty = arg_type_from_decls(n, decls);
                        let fortran_noalias = arg_is_fortran_noalias(n, decls);
                        let uses_descriptor = arg_uses_descriptor_from_decls(n, decls);
                        if arg_has_value_attr(n, decls) {
                            Some(Param {
                                name: n.clone(),
                                ty: elem_ty,
                                id: ValueId(i as u32 + 1),
                                fortran_noalias: false,
                            })
                        } else {
                            Some(Param {
                                name: n.clone(),
                                ty: if uses_descriptor {
                                    IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)))
                                } else {
                                    IrType::Ptr(Box::new(elem_ty))
                                },
                                id: ValueId(i as u32 + 1),
                                fortran_noalias,
                            })
                        }
                    } else { None }
                }).collect();
                let mut params = vec![sret];
                params.extend(real);
                (params, IrType::Void)
            } else {
                let ret_ty = return_type.as_ref()
                    .map(lower_type_spec)
                    .unwrap_or_else(|| {
                        let result_name = result.as_deref().unwrap_or(name.as_str());
                        arg_type_from_decls(result_name, decls)
                    });
                let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                    if let DummyArg::Name(n) = arg {
                        let elem_ty = arg_type_from_decls(n, decls);
                        let fortran_noalias = arg_is_fortran_noalias(n, decls);
                        let uses_descriptor = arg_uses_descriptor_from_decls(n, decls);
                        if arg_has_value_attr(n, decls) {
                            Some(Param {
                                name: n.clone(),
                                ty: elem_ty,
                                id: ValueId(i as u32),
                                fortran_noalias: false,
                            })
                        } else {
                            Some(Param {
                                name: n.clone(),
                                ty: if uses_descriptor {
                                    IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)))
                                } else {
                                    IrType::Ptr(Box::new(elem_ty))
                                },
                                id: ValueId(i as u32),
                                fortran_noalias,
                            })
                        }
                    } else { None }
                }).collect();
                (params, ret_ty)
            };

            let mut func = Function::new(func_name.clone(), func_params, ir_ret_ty.clone());
            // Propagate PURE/ELEMENTAL from AST prefix.
            use crate::ast::unit::Prefix;
            func.is_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            func.is_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            func.internal_only = internal_only;
            let mut ctx = LowerCtx::new(st, globals, type_layouts, alloc_return_funcs, optional_params, descriptor_params, internal_funcs, elemental_funcs);
            ctx.is_alloc_return = is_alloc_return;
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();

            // Build param_info skipping the sret param (it's not a Fortran variable).
            let param_info: Vec<(String, ValueId, IrType, bool)> = func.params.iter()
                .filter(|p| p.name != "_sret")
                .map(|p| {
                    let pname = p.name.to_lowercase();
                    let elem_ty = arg_type_from_decls(&pname, decls);
                    let is_value = arg_has_value_attr(&pname, decls);
                    (pname, p.id, elem_ty, is_value)
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
                        let uses_descriptor = arg_uses_descriptor_from_decls(pname, decls);
                        let slot = if uses_descriptor {
                            b.alloca(IrType::Ptr(Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384))))
                        } else {
                            b.alloca(IrType::Ptr(Box::new(elem_ty.clone())))
                        };
                        b.store(*pid, slot);
                        let dt_name = arg_derived_type_name(pname, decls);
                        ctx.locals.insert(pname.clone(), LocalInfo {
                            addr: slot, ty: elem_ty.clone(),
                            dims: arg_dims_from_decls(pname, decls, &visible_param_consts), allocatable: false, descriptor_arg: uses_descriptor, by_ref: true,
                            char_kind: CharKind::None, derived_type: dt_name, inline_const: None, is_pointer: false,
                        });
                    }
                }

                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();

                if is_alloc_return {
                    // The sret param (ValueId 0) IS the descriptor address.
                    // Pre-insert the result variable as an allocatable backed by that
                    // descriptor so alloc_decls skips it (locals.contains_key → continue).
                    let elem_ty = arg_type_from_decls(&result_name, decls);
                    ctx.locals.insert(result_name.clone(), LocalInfo {
                        addr: ValueId(0),
                        ty: elem_ty,
                        dims: vec![],
                        allocatable: true,
                        descriptor_arg: false,
                        by_ref: false,
                        char_kind: CharKind::None,
                        derived_type: None,
                        inline_const: None, is_pointer: false,
                    });
                    // result_addr = None; is_alloc_return = true tells Stmt::Return to emit ret void.
                } else {
                    let result_addr = b.alloca(ir_ret_ty.clone());
                    ctx.insert_scalar(result_name, result_addr, ir_ret_ty.clone());
                    ctx.result_addr = Some(result_addr);
                    ctx.result_type = Some(ir_ret_ty.clone());
                }

                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(&mut b, &mut ctx.locals, decls);
                alloc_decls(&mut b, &mut ctx.locals, decls, &visible_param_consts, type_layouts, &mut pending_globals, &func_name);
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    host_module,
                    ctx.st,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses);
                check_no_filtered_refs(body, &ctx.filtered_names);
                init_decls(&mut b, &ctx.locals, decls, st);
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                lower_stmts(&mut b, &mut ctx, body);

                if b.func().block(b.current_block()).terminator.is_none() {
                    let skip = if is_alloc_return { Some(ValueId(0)) } else { None };
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts, skip);
                    if is_alloc_return {
                        b.ret(None);
                    } else {
                        let result_addr = ctx.result_addr.expect("non-sret function has result_addr");
                        let rv = b.load(result_addr);
                        b.ret(Some(rv));
                    }
                }
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower nested CONTAINS subprograms.
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    true,
                );
            }
        }
        ProgramUnit::Module { decls, uses, contains, .. } => {
            // Module globals are installed in pass 1 (collect_module_globals).
            // The module body has no executable statements, but its CONTAINS
            // subprograms (module procedures) must be lowered as top-level
            // functions so they are emitted into the object file.
            let visible_param_consts = collect_decl_param_consts_with_host(decls, host_param_consts);
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let module_name = match &unit.node {
                ProgramUnit::Module { name, .. } => Some(name.as_str()),
                _ => None,
            };
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    module_name,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    false,
                );
            }
        }
        _ => {}
    }
}

/// Emit IR instructions that materialize a folded constant
/// scalar at the given target type. Used by Maj4 parameter
/// inlining: when an `Expr::Name` references a parameter whose
/// initializer const-folds, we emit `b.const_i32(value)` (or
/// the appropriate width) directly instead of going through a
/// global address + load.
fn materialize_const_scalar(b: &mut FuncBuilder, c: ConstScalar, target: &IrType) -> ValueId {
    match (c, target) {
        (ConstScalar::Int(i), IrType::Int(IntWidth::I128)) => b.const_i128(i),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I64)) => b.const_i64(i as i64),
        (ConstScalar::Int(i), IrType::Int(_)) => b.const_i32(i as i32),
        (ConstScalar::Int(i), IrType::Bool) => b.const_bool(i != 0),
        (ConstScalar::Int(i), IrType::Float(FloatWidth::F64)) => b.const_f64(i as f64),
        (ConstScalar::Int(i), IrType::Float(FloatWidth::F32)) => b.const_f32(i as f32),
        (ConstScalar::Float(f), IrType::Float(FloatWidth::F64)) => b.const_f64(f),
        (ConstScalar::Float(f), IrType::Float(FloatWidth::F32)) => b.const_f32(f as f32),
        (ConstScalar::Float(f), IrType::Int(IntWidth::I128)) => b.const_i128(f as i128),
        (ConstScalar::Float(f), IrType::Int(IntWidth::I64)) => b.const_i64(f as i64),
        (ConstScalar::Float(f), IrType::Int(_)) => b.const_i32(f as i32),
        // Fallback — emit a zero of the target's class.
        _ => b.const_i32(0),
    }
}

/// Sign-extend an i64 const value at the target IR type's width.
/// `integer(kind=1) :: x = 256` parses to 256, which doesn't fit
/// in i8; the user almost certainly meant the truncation
/// (`256 mod 256 = 0`). Clamp by masking to the low N bits and
/// re-sign-extending. Out-of-range floats and aggregates are
/// passed through unchanged. Audit CRITICAL-2.
fn clamp_const_to_type(v: ConstScalar, target: &IrType) -> ConstScalar {
    match (v, target) {
        (ConstScalar::Int(i), IrType::Int(IntWidth::I8)) => {
            ConstScalar::Int((i as i8) as i128)
        }
        (ConstScalar::Int(i), IrType::Int(IntWidth::I16)) => {
            ConstScalar::Int((i as i16) as i128)
        }
        (ConstScalar::Int(i), IrType::Int(IntWidth::I32)) => {
            ConstScalar::Int((i as i32) as i128)
        }
        (ConstScalar::Int(i), IrType::Int(IntWidth::I64)) => {
            ConstScalar::Int((i as i64) as i128)
        }
        (ConstScalar::Int(i), IrType::Bool) => {
            ConstScalar::Int(if i != 0 { 1 } else { 0 })
        }
        // Int → Float (e.g. `real :: x = 1`).
        (ConstScalar::Int(i), IrType::Float(_)) => ConstScalar::Float(i as f64),
        _ => v,
    }
}

/// Try to evaluate a scalar initializer expression at compile time
/// to a `GlobalInit`. Used by SAVE-promotion in `alloc_decls`.
///
/// Handles literals, unary minus, parenthesization, and binary
/// arithmetic (`+`, `-`, `*`, `/`, `**`) on any combination of
/// integer and real operands, plus references to named PARAMETERs
/// declared earlier in the same scope (looked up via `param_consts`).
/// Mixed int/real promotes to real per Fortran's usual arithmetic
/// rules. Anything that can't be folded (function calls, derived
/// types, strings, names that aren't compile-time parameters)
/// returns `None`. The caller then falls back to alloca + runtime
/// store, which DOES break SAVE semantics — every new non-foldable
/// case is a silent off-spec wrong-result, so the folder should
/// cover as much as possible.
fn eval_const_global_init(
    e: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
    target: Option<&IrType>,
) -> Option<GlobalInit> {
    eval_const_scalar(e, param_consts).map(|raw| {
        let clamped = match target {
            Some(t) => clamp_const_to_type(raw, t),
            None => raw,
        };
        match clamped {
            ConstScalar::Int(i) => GlobalInit::Int(i),
            ConstScalar::Float(f) => GlobalInit::Float(f),
        }
    })
}

/// Internal const-folding result for initializer expressions.
/// Int is used for integer kinds AND logical (0/1). Float is
/// used for real/double precision.
#[derive(Debug, Clone, Copy)]
enum ConstScalar {
    Int(i128),
    Float(f64),
}

impl ConstScalar {
    fn to_float(self) -> f64 {
        match self { ConstScalar::Int(i) => i as f64, ConstScalar::Float(f) => f }
    }
}

fn eval_const_scalar(
    e: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<ConstScalar> {
    use crate::ast::expr::{UnaryOp, BinaryOp};
    match &e.node {
        Expr::IntegerLiteral { text, .. } => text.parse::<i128>().ok().map(ConstScalar::Int),
        Expr::RealLiteral { text, .. } => {
            text.replace('d', "e").replace('D', "E").parse::<f64>().ok().map(ConstScalar::Float)
        }
        Expr::LogicalLiteral { value, .. } => {
            Some(ConstScalar::Int(if *value { 1 } else { 0 }))
        }
        // Audit CRITICAL-1: a name reference resolves only if it's
        // a compile-time parameter declared earlier in the same
        // scope. Anything else (regular local, dummy arg, module
        // global) is not a compile-time constant and the folder
        // gives up — the caller falls back to runtime evaluation.
        Expr::Name { name } => {
            param_consts.get(&name.to_lowercase()).copied()
        }
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_scalar(operand, param_consts)?;
            match op {
                UnaryOp::Minus => Some(match v {
                    ConstScalar::Int(i) => ConstScalar::Int(-i),
                    ConstScalar::Float(f) => ConstScalar::Float(-f),
                }),
                UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let lv = eval_const_scalar(left, param_consts)?;
            let rv = eval_const_scalar(right, param_consts)?;
            // Promote to float when either operand is float.
            let any_float = matches!(lv, ConstScalar::Float(_))
                || matches!(rv, ConstScalar::Float(_));
            if any_float {
                let l = lv.to_float();
                let r = rv.to_float();
                match op {
                    BinaryOp::Add => Some(ConstScalar::Float(l + r)),
                    BinaryOp::Sub => Some(ConstScalar::Float(l - r)),
                    BinaryOp::Mul => Some(ConstScalar::Float(l * r)),
                    // Audit Min-5: fold all IEEE 754 cases. Float
                    // division by zero now folds to ±Inf or NaN
                    // (matching `f64::powf`, which already folds
                    // negative-base fractional powers to NaN).
                    // Consistent with gfortran's `parameter ::
                    // x = 1.0/0.0 → +inf` behavior.
                    BinaryOp::Div => Some(ConstScalar::Float(l / r)),
                    BinaryOp::Pow => Some(ConstScalar::Float(l.powf(r))),
                    _ => None,
                }
            } else {
                let (ConstScalar::Int(l), ConstScalar::Int(r)) = (lv, rv) else { return None };
                match op {
                    BinaryOp::Add => Some(ConstScalar::Int(l.wrapping_add(r))),
                    BinaryOp::Sub => Some(ConstScalar::Int(l.wrapping_sub(r))),
                    BinaryOp::Mul => Some(ConstScalar::Int(l.wrapping_mul(r))),
                    BinaryOp::Div => {
                        if r == 0 { None } else { Some(ConstScalar::Int(l / r)) }
                    }
                    BinaryOp::Pow => {
                        // Integer power with non-negative exponent.
                        if r < 0 || r > i32::MAX as i128 { return None; }
                        let mut acc: i128 = 1;
                        for _ in 0..r { acc = acc.wrapping_mul(l); }
                        Some(ConstScalar::Int(acc))
                    }
                    _ => None,
                }
            }
        }
        Expr::ParenExpr { inner } => eval_const_scalar(inner, param_consts),
        _ => None,
    }
}

fn collect_decl_param_consts(
    decls: &[crate::ast::decl::SpannedDecl],
) -> HashMap<String, ConstScalar> {
    collect_decl_param_consts_with_host(decls, &HashMap::new())
}

fn collect_decl_param_consts_with_host(
    decls: &[crate::ast::decl::SpannedDecl],
    host_param_consts: &HashMap<String, ConstScalar>,
) -> HashMap<String, ConstScalar> {
    let mut param_consts: HashMap<String, ConstScalar> = host_param_consts.clone();
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { attrs, entities, .. } => {
                let is_param = attrs
                    .iter()
                    .any(|a| matches!(a, crate::ast::decl::Attribute::Parameter));
                if !is_param {
                    continue;
                }
                for entity in entities {
                    if let Some(init) = &entity.init {
                        if let Some(val) = eval_const_scalar(init, &param_consts) {
                            param_consts.insert(entity.name.to_lowercase(), val);
                        }
                    }
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, expr) in pairs {
                    if let Some(val) = eval_const_scalar(expr, &param_consts) {
                        param_consts.insert(name.to_lowercase(), val);
                    }
                }
            }
            _ => {}
        }
    }
    param_consts
}

fn const_scalar_ir_type(value: ConstScalar) -> IrType {
    match value {
        ConstScalar::Int(v) => {
            if i32::try_from(v).is_ok() {
                IrType::Int(IntWidth::I32)
            } else {
                IrType::Int(IntWidth::I64)
            }
        }
        ConstScalar::Float(_) => IrType::Float(FloatWidth::F64),
    }
}

fn install_host_param_consts(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    host_param_consts: &HashMap<String, ConstScalar>,
) {
    for (name, value) in host_param_consts {
        if locals.contains_key(name) {
            continue;
        }
        let ty = const_scalar_ir_type(*value);
        let addr = b.alloca(ty.clone());
        locals.insert(
            name.clone(),
            LocalInfo {
                addr,
                ty,
                dims: vec![],
                allocatable: false,
                descriptor_arg: false,
                by_ref: false,
                char_kind: CharKind::None,
                derived_type: None,
                inline_const: Some(*value),
                is_pointer: false,
            },
        );
    }
}

/// A pending global variable produced by the lowerer for a SAVE'd
/// scalar local. Flushed into the IR Module after the containing
/// function finishes lowering.
struct PendingGlobal {
    global: Global,
}

/// Synthesize a unique global symbol name for a SAVE'd local.
/// Audit Min-2: previously used `__save_` but the leading double
/// underscore is reserved for implementation symbols by Mach-O
/// (and by POSIX). Switched to `afs_save_` which makes the
/// provenance obvious and avoids the reserved-prefix footgun.
fn save_global_name(func_name: &str, local_name: &str) -> String {
    format!("afs_save_{}_{}", func_name.to_lowercase(), local_name.to_lowercase())
}

/// Collect the set of ValueIds whose defining instruction is a
/// `GlobalAddr`. Used by `init_decls` to skip re-initializing
/// SAVE-promoted locals on every function call. One pre-pass over
/// the function beats the O(N²) per-local scan the original
/// implementation did (Audit Maj-3).
fn collect_global_addr_values(b: &FuncBuilder) -> HashSet<ValueId> {
    let mut set = HashSet::new();
    for block in &b.func().blocks {
        for inst in &block.insts {
            if matches!(inst.kind, InstKind::GlobalAddr(_)) {
                set.insert(inst.id);
            }
        }
    }
    set
}

/// Install module-level globals as `LocalInfo` entries in the
/// function-local map so `Expr::Name` lookups can resolve them
/// uniformly with stack locals. Must run *after* `alloc_decls` so
/// that any same-named local declared in this subprogram shadows
/// the global per Fortran scoping rules.
/// Walk a body of statements and check every Expr::Name against
/// the function's filtered names set. If any match, emit a hard
/// compile-time error mentioning the filtered name. This is the
/// pre-lowering hook for audit MAJOR-1: USE ONLY hides a name
/// must not silently lower to const_int 0.
fn check_no_filtered_refs(
    body: &[crate::ast::stmt::SpannedStmt],
    filtered: &HashSet<String>,
) {
    if filtered.is_empty() { return; }
    for stmt in body {
        check_filtered_in_stmt(stmt, filtered);
    }
}

/// Walk every Stmt variant and recurse into substatements + every
/// expression-bearing field. Audit5 MAJOR-2: the original walker
/// only covered Assignment/Print/Write/Read/If/Do/Call/Block, so
/// filtered USE ONLY refs slipped through WHERE constructs, FORALL,
/// SELECT CASE, SELECT TYPE, ASSOCIATE, ALLOCATE/DEALLOCATE
/// argument exprs, IO control specifiers, and ALL of the executable
/// transfer-of-control statements that carry expressions
/// (STOP code, RETURN value, ARITHMETIC IF, COMPUTED GOTO).
fn check_filtered_in_stmt(
    stmt: &crate::ast::stmt::SpannedStmt,
    filtered: &HashSet<String>,
) {
    use crate::ast::stmt::Stmt;
    match &stmt.node {
        // ---- Assignment ----
        Stmt::Assignment { target, value }
        | Stmt::PointerAssignment { target, value } => {
            check_filtered_in_expr(target, filtered);
            check_filtered_in_expr(value, filtered);
        }

        // ---- IF ----
        Stmt::IfConstruct { condition, then_body, else_ifs, else_body, .. } => {
            check_filtered_in_expr(condition, filtered);
            check_no_filtered_refs(then_body, filtered);
            for (cond, body) in else_ifs {
                check_filtered_in_expr(cond, filtered);
                check_no_filtered_refs(body, filtered);
            }
            if let Some(eb) = else_body {
                check_no_filtered_refs(eb, filtered);
            }
        }
        Stmt::IfStmt { condition, action } => {
            check_filtered_in_expr(condition, filtered);
            check_filtered_in_stmt(action, filtered);
        }

        // ---- DO loops ----
        Stmt::DoLoop { start, end, step, body, .. } => {
            if let Some(e) = start { check_filtered_in_expr(e, filtered); }
            if let Some(e) = end { check_filtered_in_expr(e, filtered); }
            if let Some(e) = step { check_filtered_in_expr(e, filtered); }
            check_no_filtered_refs(body, filtered);
        }
        Stmt::DoWhile { condition, body, .. } => {
            check_filtered_in_expr(condition, filtered);
            check_no_filtered_refs(body, filtered);
        }
        Stmt::DoConcurrent { controls, mask, body, .. } => {
            for c in controls {
                check_filtered_in_expr(&c.start, filtered);
                check_filtered_in_expr(&c.end, filtered);
                if let Some(s) = &c.step { check_filtered_in_expr(s, filtered); }
            }
            if let Some(m) = mask { check_filtered_in_expr(m, filtered); }
            check_no_filtered_refs(body, filtered);
        }

        // ---- SELECT ----
        Stmt::SelectCase { selector, cases, .. } => {
            check_filtered_in_expr(selector, filtered);
            for case in cases {
                for sel in &case.selectors {
                    use crate::ast::stmt::CaseSelector;
                    match sel {
                        CaseSelector::Value(e) => check_filtered_in_expr(e, filtered),
                        CaseSelector::Range { low, high } => {
                            if let Some(e) = low { check_filtered_in_expr(e, filtered); }
                            if let Some(e) = high { check_filtered_in_expr(e, filtered); }
                        }
                        CaseSelector::Default => {}
                    }
                }
                check_no_filtered_refs(&case.body, filtered);
            }
        }
        Stmt::SelectType { selector, guards, .. } => {
            check_filtered_in_expr(selector, filtered);
            for guard in guards {
                use crate::ast::stmt::TypeGuard;
                let body = match guard {
                    TypeGuard::TypeIs { body, .. }
                    | TypeGuard::ClassIs { body, .. }
                    | TypeGuard::ClassDefault { body } => body,
                };
                check_no_filtered_refs(body, filtered);
            }
        }

        // ---- WHERE / FORALL ----
        Stmt::WhereConstruct { mask, body, elsewhere, .. } => {
            check_filtered_in_expr(mask, filtered);
            check_no_filtered_refs(body, filtered);
            for (mcond, ebody) in elsewhere {
                if let Some(m) = mcond { check_filtered_in_expr(m, filtered); }
                check_no_filtered_refs(ebody, filtered);
            }
        }
        Stmt::WhereStmt { mask, stmt } => {
            check_filtered_in_expr(mask, filtered);
            check_filtered_in_stmt(stmt, filtered);
        }
        Stmt::ForallConstruct { specs, mask, body, .. } => {
            for s in specs {
                check_filtered_in_expr(&s.start, filtered);
                check_filtered_in_expr(&s.end, filtered);
                if let Some(st) = &s.step { check_filtered_in_expr(st, filtered); }
            }
            if let Some(m) = mask { check_filtered_in_expr(m, filtered); }
            check_no_filtered_refs(body, filtered);
        }
        Stmt::ForallStmt { specs, mask, stmt } => {
            for s in specs {
                check_filtered_in_expr(&s.start, filtered);
                check_filtered_in_expr(&s.end, filtered);
                if let Some(st) = &s.step { check_filtered_in_expr(st, filtered); }
            }
            if let Some(m) = mask { check_filtered_in_expr(m, filtered); }
            check_filtered_in_stmt(stmt, filtered);
        }

        // ---- BLOCK / ASSOCIATE ----
        Stmt::Block { body, .. } => check_no_filtered_refs(body, filtered),
        Stmt::Associate { assocs, body, .. } => {
            for (_, e) in assocs {
                check_filtered_in_expr(e, filtered);
            }
            check_no_filtered_refs(body, filtered);
        }

        // ---- Branch / transfer ----
        Stmt::Stop { code, .. } | Stmt::ErrorStop { code, .. } => {
            if let Some(e) = code { check_filtered_in_expr(e, filtered); }
        }
        Stmt::Return { value } => {
            if let Some(e) = value { check_filtered_in_expr(e, filtered); }
        }
        Stmt::ComputedGoto { selector, .. } => {
            check_filtered_in_expr(selector, filtered);
        }
        Stmt::ArithmeticIf { expr, .. } => {
            check_filtered_in_expr(expr, filtered);
        }
        Stmt::Exit { .. }
        | Stmt::Cycle { .. }
        | Stmt::Goto { .. }
        | Stmt::Continue { .. } => {}
        Stmt::Labeled { stmt: inner, .. } => {
            check_no_filtered_refs(std::slice::from_ref(inner.as_ref()), filtered);
        }

        // ---- I/O ----
        Stmt::Print { format, items } => {
            check_filtered_in_expr(format, filtered);
            for item in items { check_filtered_in_expr(item, filtered); }
        }
        Stmt::Write { controls, items } | Stmt::Read { controls, items } => {
            for c in controls { check_filtered_in_expr(&c.value, filtered); }
            for item in items { check_filtered_in_expr(item, filtered); }
        }
        Stmt::Open { specs }
        | Stmt::Close { specs }
        | Stmt::Rewind { specs }
        | Stmt::Backspace { specs }
        | Stmt::Endfile { specs }
        | Stmt::Flush { specs }
        | Stmt::Wait { specs } => {
            for c in specs { check_filtered_in_expr(&c.value, filtered); }
        }
        Stmt::Inquire { specs, items } => {
            for c in specs { check_filtered_in_expr(&c.value, filtered); }
            for item in items { check_filtered_in_expr(item, filtered); }
        }

        // ---- Memory ----
        Stmt::Allocate { items, opts } | Stmt::Deallocate { items, opts } => {
            for item in items { check_filtered_in_expr(item, filtered); }
            for c in opts { check_filtered_in_expr(&c.value, filtered); }
        }
        Stmt::Nullify { items } => {
            for item in items { check_filtered_in_expr(item, filtered); }
        }

        // ---- Other executable ----
        Stmt::Call { callee, args } => {
            check_filtered_in_expr(callee, filtered);
            for a in args { check_filtered_in_subscript(&a.value, filtered); }
        }
        Stmt::Namelist { .. } => {}
        Stmt::Declaration(_) => {
            // Initializers in inline declarations could reference
            // module names, but Decl init exprs go through a
            // separate const-fold path that already errors on
            // unknown names. Conservative: skip here.
        }
    }
}

fn check_filtered_in_expr(
    expr: &crate::ast::expr::SpannedExpr,
    filtered: &HashSet<String>,
) {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            if filtered.contains(&key) {
                eprintln!(
                    "armfortas: error: {}:{}: '{}' is not accessible in this scope — \
                     it was filtered out by a USE ONLY clause (audit MAJOR-1)",
                    expr.span.start.line,
                    expr.span.start.col,
                    name,
                );
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
        }
        Expr::ComponentAccess { base, .. } => {
            check_filtered_in_expr(base, filtered);
        }
        Expr::BinaryOp { left, right, .. } => {
            check_filtered_in_expr(left, filtered);
            check_filtered_in_expr(right, filtered);
        }
        Expr::UnaryOp { operand, .. } => check_filtered_in_expr(operand, filtered),
        Expr::ParenExpr { inner } => check_filtered_in_expr(inner, filtered),
        Expr::FunctionCall { callee, args } => {
            check_filtered_in_expr(callee, filtered);
            for a in args { check_filtered_in_subscript(&a.value, filtered); }
        }
        Expr::ArrayConstructor { values, .. } => {
            for v in values { check_filtered_in_acvalue(v, filtered); }
        }
        Expr::ComplexLiteral { real, imag } => {
            check_filtered_in_expr(real, filtered);
            check_filtered_in_expr(imag, filtered);
        }
        // Pure literals: nothing to walk.
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::BozLiteral { .. } => {}
    }
}

fn check_filtered_in_subscript(
    sub: &crate::ast::expr::SectionSubscript,
    filtered: &HashSet<String>,
) {
    use crate::ast::expr::SectionSubscript;
    match sub {
        SectionSubscript::Element(e) => check_filtered_in_expr(e, filtered),
        SectionSubscript::Range { start, end, stride } => {
            if let Some(e) = start { check_filtered_in_expr(e, filtered); }
            if let Some(e) = end { check_filtered_in_expr(e, filtered); }
            if let Some(e) = stride { check_filtered_in_expr(e, filtered); }
        }
    }
}

fn check_filtered_in_acvalue(
    v: &crate::ast::expr::AcValue,
    filtered: &HashSet<String>,
) {
    use crate::ast::expr::AcValue;
    match v {
        AcValue::Expr(e) => check_filtered_in_expr(e, filtered),
        AcValue::ImpliedDo(ido) => {
            for inner in &ido.values { check_filtered_in_acvalue(inner, filtered); }
            check_filtered_in_expr(&ido.start, filtered);
            check_filtered_in_expr(&ido.end, filtered);
            if let Some(s) = &ido.step { check_filtered_in_expr(s, filtered); }
        }
    }
}

/// Walk the function's USE statements and collect every name
/// from a USE-only-imported module that the only-list filtered
/// out. Audit MAJOR-1: those names must NOT silently fall
/// through to const_int 0; the lowerer treats them as undefined
/// at the reference site.
fn compute_filtered_names(
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    uses: &[crate::ast::decl::SpannedDecl],
) -> HashSet<String> {
    use crate::ast::decl::OnlyItem;
    let mut filtered: HashSet<String> = HashSet::new();
    for decl in uses {
        let Decl::UseStmt { module, only: Some(only_list), .. } = &decl.node else { continue; };
        let mod_key = module.to_lowercase();
        // The set of names this module exports (limited to what
        // collect_module_globals registered — module functions and
        // derived types are tracked elsewhere and remain visible).
        let mut exports: HashSet<String> = HashSet::new();
        for (mk, var) in globals.keys() {
            if *mk == mod_key {
                exports.insert(var.clone());
            }
        }
        // The set of (lowercase) names the only-list explicitly
        // imports. A rename's `remote` is what's pulled from the
        // module; a Name is itself.
        let mut imported: HashSet<String> = HashSet::new();
        for item in only_list {
            match item {
                OnlyItem::Name(n) => { imported.insert(n.to_lowercase()); }
                OnlyItem::Rename(rn) => { imported.insert(rn.remote.to_lowercase()); }
            }
        }
        // Anything in exports but not imported is now filtered.
        for e in &exports {
            if !imported.contains(e) {
                filtered.insert(e.clone());
            }
        }
    }
    filtered
}

/// Install a module-level global as a `LocalInfo` entry under the
/// given local key. Shared helper so all install paths build a
/// consistent LocalInfo shape.
fn install_one_global(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    local_key: String,
    info: &ModuleGlobalInfo,
) {
    if locals.contains_key(&local_key) { return; }
    // For allocatable module arrays the global is a 384-byte
    // descriptor — global_addr produces a `Ptr<Array<i8, 384>>`
    // which feeds the runtime allocate/deallocate/subscript
    // helpers as the descriptor address.
    let addr_ty = if info.allocatable {
        IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)
    } else {
        info.ty.clone()
    };
    let addr = b.global_addr(&info.symbol, addr_ty);
    locals.insert(local_key, LocalInfo {
        addr,
        ty: info.ty.clone(),
        dims: info.dims.clone(),
        allocatable: info.allocatable,
        descriptor_arg: false,
        by_ref: false,
        char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false,
    });
}

/// Install module globals imported by this function's USE
/// statements as `LocalInfo` entries. Honors:
///   * USE ONLY filtering — only names in the only-list are installed
///   * Renames — both forms, `use m, only: y => x` and
///     `use m, x => y` (non-only rename)
///   * Cross-module collision detection — if two modules bring in
///     the same local key through their use list, the emitted IR
///     would resolve ambiguously; we skip the second one and note
///     the collision in an eprintln (sema doesn't yet diagnose).
///
/// Audit C2/C3/C4: previously this function installed every
/// global regardless of any USE statement, ignored ONLY filtering,
/// silently dropped USE renames, and let two same-named variables
/// from different modules silently overwrite each other.
fn install_globals_as_locals(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    uses: &[crate::ast::decl::SpannedDecl],
    host_module: Option<&str>,
    st: &SymbolTable,
) {
    use crate::ast::decl::OnlyItem;

    // Sorted per-use iteration so the emitted global_addr
    // instructions land in deterministic order. Audit B-3 holds
    // across this path too.
    //
    // The two-pass pattern:
    //   1. Enumerate the (use statement, key-in-local-scope, module_key)
    //      triples this function imports.
    //   2. Sort by local-scope key.
    //   3. Install in order, checking for collision before inserting.
    let mut pending: Vec<(String, (String, String))> = Vec::new();

    if let Some(module_name) = host_module {
        let mod_key = module_name.to_lowercase();
        for (mk, var) in globals.keys() {
            if *mk == mod_key {
                pending.push((var.clone(), (mod_key.clone(), var.clone())));
            }
        }
    }

    for decl in uses {
        let Decl::UseStmt { module, nature: _, renames, only } = &decl.node else { continue; };
        let mod_key = module.to_lowercase();
        if let Some(only_list) = only {
            for item in only_list {
                match item {
                    OnlyItem::Name(n) => {
                        let n_lc = n.to_lowercase();
                        pending.push((n_lc.clone(), (mod_key.clone(), n_lc)));
                    }
                    OnlyItem::Rename(rn) => {
                        pending.push((
                            rn.local.to_lowercase(),
                            (mod_key.clone(), rn.remote.to_lowercase()),
                        ));
                    }
                }
            }
        } else {
            // No ONLY list: import every name from the module,
            // minus any rename targets (which are substituted).
            let rename_targets: std::collections::HashSet<String> =
                renames.iter().map(|r| r.remote.to_lowercase()).collect();
            for (mk, var) in globals.keys() {
                if *mk != mod_key { continue; }
                if rename_targets.contains(var) { continue; }
                pending.push((var.clone(), (mod_key.clone(), var.clone())));
            }
            for rn in renames {
                pending.push((
                    rn.local.to_lowercase(),
                    (mod_key.clone(), rn.remote.to_lowercase()),
                ));
            }
        }
    }

    pending.sort_by(|a, b| a.0.cmp(&b.0));

    let mut installed_from: HashMap<String, String> = HashMap::new();
    for (local_key, (mod_key, var_key)) in pending {
        if let Some(info) = globals.get(&(mod_key.clone(), var_key.clone())) {
            // Collision check: two modules exporting the same local key.
            if let Some(prev_mod) = installed_from.get(&local_key) {
                if *prev_mod != mod_key {
                    eprintln!(
                        "warning: ambiguous USE import '{}' from both '{}' and '{}'; \
                         keeping the first",
                        local_key, prev_mod, mod_key,
                    );
                    continue;
                }
            }
            installed_from.insert(local_key.clone(), mod_key);
            install_one_global(b, locals, local_key, info);
        } else {
            // Not an IR global — check if it's an intrinsic module parameter constant
            // (iso_c_binding, iso_fortran_env). These are registered in the symbol
            // table but never emitted as IR globals; install them as inline_const locals.
            if locals.contains_key(&local_key) { continue; }
            if let Some(mod_scope_id) = st.find_module_scope(&mod_key) {
                if let Some(sym) = st.scope(mod_scope_id).symbols.get(&var_key) {
                    if sym.attrs.parameter {
                        if let Some(cv) = sym.const_value {
                            let ty = IrType::Int(IntWidth::I32);
                            // Create a dummy alloca (never loaded from; inline_const
                            // short-circuits at every use site via materialize_const_scalar).
                            let addr = b.alloca(ty.clone());
                            locals.insert(local_key.clone(), LocalInfo {
                                addr,
                                ty,
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: None,
                                inline_const: Some(ConstScalar::Int(cv as i128)),
                                is_pointer: false,
                            });
                            installed_from.insert(local_key, mod_key);
                        }
                    }
                }
            }
            // If still not found (e.g., USE references a name that doesn't exist),
            // skip silently — sema should have diagnosed it.
        }
    }
}

/// Allocate local variables from declarations. Handles both scalars and arrays.
fn alloc_decls(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
    visible_param_consts: &HashMap<String, ConstScalar>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    pending_globals: &mut Vec<PendingGlobal>,
    func_name: &str,
) {
    use crate::ast::decl::Attribute;

    // Pre-scan standalone PARAMETER statements so a TypeDecl entity
    // whose value comes from a separate `parameter (name = expr)`
    // statement still triggers SAVE-promotion at alloc time. Without
    // this pre-scan, the standalone form would silently fall back to
    // the alloca + per-call store path.
    let mut parameter_inits: HashMap<String, &crate::ast::expr::SpannedExpr> = HashMap::new();
    for d in decls {
        if let Decl::ParameterStmt { pairs } = &d.node {
            for (name, expr) in pairs {
                parameter_inits.insert(name.to_lowercase(), expr);
            }
        }
    }

    // Audit CRITICAL-1: build the per-scope parameter constants
    // table so SAVE-promotion's eval_const_global_init can resolve
    // `Expr::Name` references against compile-time-known parameters
    // declared earlier in the same scope. Without this, an init
    // like `integer :: x = k * 2` (k a parameter) silently falls
    // back to alloca + per-call store and breaks SAVE semantics.
    //
    // Parameters can reference earlier parameters (`tau = 2 * pi`),
    // so we walk decls in order and build the map incrementally.
    let param_consts = collect_decl_param_consts_with_host(decls, visible_param_consts);

    for decl in decls {
        if let Decl::TypeDecl { type_spec, attrs, entities } = &decl.node {
            let elem_ty = lower_type_spec(type_spec);

            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let Attribute::Dimension(specs) = a { Some(specs) } else { None }
            });
            let is_allocatable = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let is_pointer_attr = attrs.iter().any(|a| matches!(a, Attribute::Pointer));

            for entity in entities {
                let key = entity.name.to_lowercase();
                if locals.contains_key(&key) { continue; }

                // Use entity-level array spec, or fall back to attribute-level DIMENSION.
                let array_spec = entity.array_spec.as_ref().or(attr_dims);

                // Check for character type.
                let char_len = match type_spec {
                    TypeSpec::Character(Some(sel)) => {
                        match &sel.len {
                            Some(crate::ast::decl::LenSpec::Expr(e)) => {
                                eval_const_int_in_scope(e, &param_consts)
                            }
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

                if is_pointer_attr && array_spec.is_some() {
                    // Pointer to array.  Reuses the 384-byte array
                    // descriptor layout that allocatables use: the
                    // pointer slot carries base_addr, elem_size,
                    // rank, flags, and per-dim bounds so that
                    // downstream subscript / SIZE / whole-array
                    // operations pick it up through the existing
                    // descriptor path.  `=>` fills the slot from a
                    // materialised descriptor of the target (see
                    // Stmt::PointerAssignment).  Unassociated state
                    // is encoded by flags=0, same as an unallocated
                    // allocatable.
                    //
                    // We set `allocatable = true` so that
                    // `local_uses_array_descriptor` and
                    // `array_descriptor_addr` treat the slot as a
                    // descriptor-at-info.addr (no extra indirection).
                    // `is_pointer = true` is separately used by
                    // scope-exit deallocation to suppress the
                    // afs_deallocate_array call — a pointer does
                    // not own its target.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384);
                    let addr = b.alloca(desc_ty);
                    let zero_byte = b.const_i32(0);
                    let size384 = b.const_i64(384);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero_byte, size384],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    // dims is left empty for a deferred-shape pointer;
                    // the descriptor carries the runtime rank and
                    // bounds after `=>` binds it to a target.
                    locals.insert(key, LocalInfo {
                        addr,
                        ty: elem_ty.clone(),
                        dims: vec![],
                        allocatable: true,
                        descriptor_arg: false,
                        by_ref: false,
                        char_kind: CharKind::None,
                        derived_type: None,
                        inline_const: None,
                        is_pointer: true,
                    });
                    continue;
                }
                if is_pointer_attr && matches!(type_spec, TypeSpec::Type(_)) && array_spec.is_none() {
                    // Pointer to derived type.  Slot holds an 8-byte
                    // pointer to the target struct; ComponentAccess
                    // loads the slot and uses that address as the
                    // struct base.  derived_type is stored so that
                    // component lookup can find the type layout.
                    if let TypeSpec::Type(ref type_name) = type_spec {
                        let addr = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        let zero_byte = b.const_i32(0);
                        let eight = b.const_i64(8);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero_byte, eight],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        locals.insert(key, LocalInfo {
                            addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: Some(type_name.clone()),
                            inline_const: None,
                            is_pointer: true,
                        });
                        continue;
                    }
                }
                if is_deferred_char && is_allocatable {
                    // Deferred-length allocatable character: 32-byte StringDescriptor.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 32);
                    let addr = b.alloca(desc_ty);
                    let zero = b.const_i32(0);
                    let size32 = b.const_i64(32);
                    b.call(FuncRef::External("memset".into()), vec![addr, zero, size32], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                    locals.insert(key, LocalInfo {
                        addr, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        dims: vec![], allocatable: true, descriptor_arg: false, by_ref: false,
                        char_kind: CharKind::Deferred, derived_type: None, inline_const: None, is_pointer: false,
                    });
                    continue;
                } else if let Some(len) = char_len {
                    if let Some(specs) = array_spec {
                        let dims = extract_array_dims(specs, &param_consts);
                        let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                        let table_ty = IrType::Array(
                            Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
                            total_size as u64,
                        );
                        let addr = b.alloca(table_ty);
                        let buf_ty = IrType::Array(
                            Box::new(IrType::Int(IntWidth::I8)),
                            (total_size * (len + 1)) as u64,
                        );
                        let buf = b.alloca(buf_ty);
                        let zero = b.const_i32(0);
                        let total_bytes = b.const_i64(total_size * (len + 1));
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![buf, zero, total_bytes],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        let space = b.const_i32(b' ' as i32);
                        let char_bytes = b.const_i64(len);
                        for idx in 0..total_size {
                            let slot_idx = b.const_i64(idx);
                            let slot_ptr = b.gep(
                                addr,
                                vec![slot_idx],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            let byte_off = b.const_i64(idx * (len + 1));
                            let elem_ptr = b.gep(buf, vec![byte_off], IrType::Int(IntWidth::I8));
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![elem_ptr, space, char_bytes],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            b.store(elem_ptr, slot_ptr);
                        }
                        locals.insert(key, LocalInfo {
                            addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims,
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::Fixed(len),
                            derived_type: None,
                            inline_const: None, is_pointer: false,
                        });
                        continue;
                    }
                    if !is_allocatable {
                        // Fixed-length character(N): alloca N+1 bytes so call-boundary
                        // lowering can rely on a stable trailing NUL while the Fortran
                        // value still occupies the first N bytes.
                        let buf_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), (len + 1) as u64);
                        let addr = b.alloca(buf_ty);
                        let zero = b.const_i32(0);
                        let total = b.const_i64(len + 1);
                        b.call(FuncRef::External("memset".into()), vec![addr, zero, total], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        // Initialize with spaces.
                        let space = b.const_i32(b' ' as i32);
                        let len_val = b.const_i64(len);
                        b.call(FuncRef::External("memset".into()), vec![addr, space, len_val], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        locals.insert(key, LocalInfo {
                            addr, ty: IrType::Int(IntWidth::I8),
                            dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false,
                            char_kind: CharKind::Fixed(len), derived_type: None, inline_const: None, is_pointer: false,
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
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: true, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
                } else if let Some(specs) = array_spec {
                    // Fixed-size array variable.
                    let dims = extract_array_dims(specs, &param_consts);
                    let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                    let elem_bytes = ir_scalar_byte_size(&elem_ty);
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
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: true, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
                    } else {
                        // Small array: stack allocation.
                        let arr_ty = IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                        let addr = b.alloca(arr_ty);
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
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
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: Some(type_name.clone()), inline_const: None, is_pointer: false,
                        });
                    } else {
                        // Unknown derived type — fall back to 8-byte alloca.
                        let addr = b.alloca(IrType::Int(IntWidth::I64));
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
                    }
                } else if is_pointer_attr && array_spec.is_none() {
                    // Scalar Fortran POINTER: allocate a pointer slot
                    // (`alloca ptr<elem_ty>`) that holds the address
                    // of whatever the pointer is currently associated
                    // with.  `=>` stores into this slot; plain `=`
                    // dereferences it; reads load twice.  The slot
                    // starts null so that ASSOCIATED() returns
                    // false before the first `=>`.
                    let addr = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                    // Memset the slot to zero so unassociated pointers
                    // compare null.  Eight bytes matches the ARM64
                    // pointer width.
                    let zero_byte = b.const_i32(0);
                    let eight = b.const_i64(8);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero_byte, eight],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    locals.insert(key, LocalInfo {
                        addr,
                        ty: elem_ty.clone(),
                        dims: vec![],
                        allocatable: false,
                        descriptor_arg: false,
                        by_ref: false,
                        char_kind: CharKind::None,
                        derived_type: None,
                        inline_const: None,
                        is_pointer: true,
                    });
                } else {
                    // Scalar variable. Three sub-cases:
                    //   (a) PARAMETER-attributed and folds → inline
                    //       at every use site. No alloca, no global,
                    //       no .data slot. Audit MAJOR-4.
                    //   (b) Has a const-evaluable init but isn't a
                    //       parameter → SAVE-promote to a module
                    //       global (F2018 §8.5.16 implicit SAVE).
                    //   (c) Plain alloca, no init.
                    let init_expr: Option<&crate::ast::expr::SpannedExpr> =
                        entity.init.as_ref().or_else(|| parameter_inits.get(&key).copied());
                    let is_parameter = attrs.iter().any(|a| matches!(a, Attribute::Parameter))
                        || parameter_inits.contains_key(&key);

                    if is_parameter {
                        // Audit MAJOR-4: pure compile-time parameter.
                        // Try to fold; if we can, store the value in
                        // inline_const and skip the global+alloca.
                        // Use a one-byte sentinel alloca for `addr`
                        // so other code paths that touch info.addr
                        // still work, but never load through it.
                        let folded = init_expr
                            .and_then(|e| eval_const_scalar(e, &param_consts))
                            .map(|raw| clamp_const_to_type(raw, &elem_ty));
                        if let Some(value) = folded {
                            // Sentinel alloca — never read.
                            let addr = b.alloca(elem_ty.clone());
                            locals.insert(key, LocalInfo {
                                addr, ty: elem_ty.clone(),
                                dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false,
                                char_kind: CharKind::None, derived_type: None,
                                inline_const: Some(value), is_pointer: false,
                            });
                            continue;
                        }
                        // Fall through to the SAVE path if the
                        // parameter init can't be folded — at least
                        // semantics are preserved.
                    }

                    if let Some(init) = init_expr.and_then(|e| eval_const_global_init(e, &param_consts, Some(&elem_ty))) {
                        let global_name = save_global_name(func_name, &key);
                        pending_globals.push(PendingGlobal {
                            global: Global {
                                name: global_name.clone(),
                                ty: elem_ty.clone(),
                                initializer: Some(init),
                            },
                        });
                        let addr = b.global_addr(&global_name, elem_ty.clone());
                        locals.insert(key, LocalInfo {
                            addr, ty: elem_ty.clone(),
                            dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false,
                            char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false,
                        });
                    } else {
                        let addr = b.alloca(elem_ty.clone());
                        locals.insert(key, LocalInfo {
                            addr, ty: elem_ty.clone(),
                            dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false,
                            char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false,
                        });
                    }
                }
            }
        }
    }
}

/// Lower initializer expressions for declared variables.
///
/// Handles two AST shapes:
///   1. `Decl::TypeDecl` entities with `entity.init` set. This
///      covers BOTH `integer :: x = 42` and
///      `integer, parameter :: pi = 3.14` — the parameter
///      attribute doesn't change the lowering, only sema's
///      classification of the symbol.
///   2. Standalone `Decl::ParameterStmt { pairs }`, where each
///      pair refers to an already-allocated local declared
///      elsewhere in the same decl list.
///
/// Most scalar locals with const-evaluable initializers are
/// SAVE-promoted to module globals back in `alloc_decls`; for
/// those, `is_global_addr` returns true and this pass leaves the
/// initialization to the .data section. The remaining cases this
/// pass handles are non-const initializers (rare).
///
/// Must run *after* `alloc_decls` so that all locals exist. Only
/// stores into scalar slots — array, character, derived-type, and
/// allocatable initializers have their own paths in alloc_decls.
fn init_decls(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
    st: &SymbolTable,
) {
    // Pre-collect the set of GlobalAddr-defining ValueIds so the
    // inner skip check is O(1). Audit Maj-3.
    let global_addr_ids = collect_global_addr_values(b);
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { entities, .. } => {
                for entity in entities {
                    let Some(init_expr) = &entity.init else { continue; };
                    let key = entity.name.to_lowercase();
                    let Some(info) = locals.get(&key) else { continue; };
                    // Dummy arguments (by_ref locals) cannot have
                    // initializers per the Fortran standard — they
                    // bind to caller storage. If sema lets one
                    // through it would be a bug; the debug_assert
                    // catches it in development without crashing
                    // release builds. Audit Min-4.
                    debug_assert!(
                        !info.by_ref,
                        "init_decls: dummy argument {:?} should not have an initializer",
                        key,
                    );
                    if info.by_ref { continue; }

                    // Array entity with an array constructor init:
                    // store each literal element into the slot.
                    // Only stack/non-allocatable arrays are handled
                    // here; allocatable arrays would need their
                    // descriptor allocated first.
                    if !info.dims.is_empty()
                        && !info.allocatable
                        && matches!(info.char_kind, CharKind::None)
                        && info.derived_type.is_none()
                    {
                        if let Expr::ArrayConstructor { values, .. } = &init_expr.node {
                            store_ac_values_into(b, locals, info.addr, &info.ty, values, st);
                        }
                        continue;
                    }

                    // Only initialize plain scalar slots; characters,
                    // allocatables, and derived types are handled
                    // elsewhere.
                    if !info.dims.is_empty()
                        || info.allocatable
                        || !matches!(info.char_kind, CharKind::None)
                        || info.derived_type.is_some()
                    {
                        continue;
                    }
                    // SAVE-promoted locals are backed by a module
                    // global already initialized at link time. Don't
                    // re-store on every call — that would defeat
                    // the SAVE semantics (audit MAJOR-1).
                    if global_addr_ids.contains(&info.addr) {
                        continue;
                    }
                    // Audit5 MAJOR-3: PARAMETER scalars folded by
                    // alloc_decls have inline_const set and a
                    // sentinel alloca that is never loaded — every
                    // use materializes the constant directly. The
                    // store here would be dead in the IR forever
                    // at -O0 (mem2reg cleans it up at -O1+, but
                    // we shouldn't generate dead code in the first
                    // place).
                    if info.inline_const.is_some() {
                        continue;
                    }
                    let val = lower_expr(b, locals, init_expr, st);
                    let coerced = coerce_to_type(b, val, &info.ty);
                    b.store(coerced, info.addr);
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, expr) in pairs {
                    let key = name.to_lowercase();
                    let Some(info) = locals.get(&key) else { continue; };
                    if !info.dims.is_empty()
                        || info.allocatable
                        || info.by_ref
                        || !matches!(info.char_kind, CharKind::None)
                        || info.derived_type.is_some()
                    {
                        continue;
                    }
                    // SAVE-promoted locals are backed by a module
                    // global; the initial value is already baked
                    // into .data at link time, so skip the runtime
                    // store. Audit MAJOR-1 interaction.
                    if global_addr_ids.contains(&info.addr) {
                        continue;
                    }
                    // Audit5 MAJOR-3: same dead-store skip as the
                    // TypeDecl arm above. Standalone PARAMETER
                    // statements also produce inline_const-tagged
                    // locals when alloc_decls successfully folds
                    // the value.
                    if info.inline_const.is_some() {
                        continue;
                    }
                    let val = lower_expr(b, locals, expr, st);
                    let coerced = coerce_to_type(b, val, &info.ty);
                    b.store(coerced, info.addr);
                }
            }
            // Audit MEDIUM-3: DATA statements. Each set pairs
            // target objects with values. For the simple form
            // `data x /42/, y /3.14/`, walk objects + values
            // pairwise and emit a store per scalar Name target.
            // Implied-do object lists and value-side repetition
            // (`r*v`) are not yet supported — they fall through
            // silently and are tracked as future work.
            Decl::DataStmt { sets } => {
                for set in sets {
                    let n = set.objects.len().min(set.values.len());
                    for (target, value) in
                        set.objects.iter().zip(set.values.iter()).take(n)
                    {
                        let Expr::Name { name } = &target.node else { continue; };
                        let key = name.to_lowercase();
                        let Some(info) = locals.get(&key) else { continue; };
                        if !info.dims.is_empty()
                            || info.allocatable
                            || info.by_ref
                            || !matches!(info.char_kind, CharKind::None)
                            || info.derived_type.is_some()
                        {
                            continue;
                        }
                        // Don't shadow a SAVE-promoted global —
                        // its initial value is in .data already.
                        if global_addr_ids.contains(&info.addr) {
                            continue;
                        }
                        let val = lower_expr(b, locals, value, st);
                        let coerced = coerce_to_type(b, val, &info.ty);
                        b.store(coerced, info.addr);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Coerce a scalar value to a target type for initializer storage.
///
/// Covers every Fortran scalar coercion that can show up at an
/// initializer-store site:
///   * Int → Int width change (sign-extend or truncate). Audit
///     Min-3: Fortran integers are always signed, so the int_extend
///     `signed` flag is hardcoded to `true`.
///   * Int ↔ Float (round to nearest for Float→Int).
///   * F32 ↔ F64 (extend / truncate).
///   * Bool ↔ Int (round-trip via int_extend; Fortran logicals
///     occupy a full kind so this is rare but legal).
///
/// Anything that doesn't match one of those cases falls into the
/// `_ => val` arm and a `debug_assert!` fires — silently passing
/// the wrong-typed value would let a future caller wire mismatched
/// types into a Store, which the verifier (after MAJOR-4) would
/// then catch much later. Better to fail loudly at the source.
fn coerce_to_type(b: &mut FuncBuilder, val: ValueId, target: &IrType) -> ValueId {
    let src = match b.func().value_type(val) {
        Some(t) => t,
        None => return val,
    };
    if src == *target {
        return val;
    }
    match (&src, target) {
        // Int → Float
        (IrType::Int(_), IrType::Float(fw)) => b.int_to_float(val, *fw),
        // Float → Int
        (IrType::Float(_), IrType::Int(iw)) => b.float_to_int(val, *iw),
        // F32 ↔ F64
        (IrType::Float(FloatWidth::F32), IrType::Float(FloatWidth::F64)) => {
            b.float_extend(val, FloatWidth::F64)
        }
        (IrType::Float(FloatWidth::F64), IrType::Float(FloatWidth::F32)) => {
            b.float_trunc(val, FloatWidth::F32)
        }
        // Int width change. Audit Min-3: Fortran integers are signed.
        (IrType::Int(src_w), IrType::Int(dst_w)) => {
            if dst_w.bits() > src_w.bits() {
                b.int_extend(val, *dst_w, true)
            } else if dst_w.bits() < src_w.bits() {
                b.int_trunc(val, *dst_w)
            } else {
                val
            }
        }
        // Bool ↔ Int via int_extend. Bool is i1 in our model.
        (IrType::Bool, IrType::Int(iw)) => b.int_extend(val, *iw, false),
        (IrType::Int(_), IrType::Bool) => b.int_trunc(val, IntWidth::I8),
        _ => {
            debug_assert!(
                false,
                "coerce_to_type: unhandled coercion {:?} → {:?}", src, target,
            );
            val
        }
    }
}

/// Extract compile-time array dimensions from array spec.
/// Returns (lower_bound, extent) pairs. Runtime expressions default to (1, 1).
fn extract_array_dims(
    specs: &[ArraySpec],
    param_consts: &HashMap<String, ConstScalar>,
) -> Vec<(i64, i64)> {
    specs.iter().map(|spec| {
        match spec {
            ArraySpec::Explicit { lower, upper } => {
                let lo = lower
                    .as_ref()
                    .and_then(|e| eval_const_int_in_scope(e, param_consts))
                    .unwrap_or(1);
                let hi = eval_const_int_in_scope(upper, param_consts).unwrap_or(1);
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

fn eval_const_int_in_scope(
    expr: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<i64> {
    match eval_const_scalar(expr, param_consts)? {
        ConstScalar::Int(v) => i64::try_from(v).ok(),
        ConstScalar::Float(_) => None,
    }
}

/// Resolve the raw data pointer and declared length for a character argument expression.
/// Returns `None` if the argument is not a recognized fixed-length character.
/// Build (ptr, len) for a substring `base_ptr(start:end)` per F2018 7.4.4.2.
/// `start` defaults to 1, `end` defaults to the base string's length.
/// Negative resulting lengths are clamped to 0 to match the standard's
/// zero-length substring semantics when `start > end`.
fn lower_substring(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    st: &SymbolTable,
    base_ptr: ValueId,
    base_len: ValueId,
    start: Option<&crate::ast::expr::SpannedExpr>,
    end: Option<&crate::ast::expr::SpannedExpr>,
) -> (ValueId, ValueId) {
    let widen = |b: &mut FuncBuilder, e: &crate::ast::expr::SpannedExpr| -> ValueId {
        let v = lower_expr(b, locals, e, st);
        match b.func().value_type(v) {
            Some(IrType::Int(IntWidth::I64)) => v,
            _ => b.int_extend(v, IntWidth::I64, true),
        }
    };
    let start_val = match start {
        Some(se) => widen(b, se),
        None => b.const_i64(1),
    };
    let end_val = match end {
        Some(ee) => widen(b, ee),
        None => base_len,
    };
    let one = b.const_i64(1);
    let off = b.isub(start_val, one);
    let sub_ptr = b.gep(base_ptr, vec![off], IrType::Int(IntWidth::I8));
    let span = b.isub(end_val, start_val);
    let raw_len = b.iadd(span, one);
    let zero = b.const_i64(0);
    let is_pos = b.icmp(CmpOp::Ge, raw_len, zero);
    let sub_len = b.select(is_pos, raw_len, zero);
    (sub_ptr, sub_len)
}

fn char_addr_and_len(
    b: &mut FuncBuilder,
    arg_spanned: &crate::ast::expr::SpannedExpr,
    locals: &HashMap<String, LocalInfo>,
) -> Option<(ValueId, i64)> {
    use crate::ast::expr::Expr;
    match &arg_spanned.node {
        Expr::Name { name } => {
            let info = locals.get(&name.to_lowercase())?;
            match &info.char_kind {
                CharKind::Fixed(n) => {
                    if !info.dims.is_empty() {
                        return None;
                    }
                    let ptr = if info.by_ref {
                        let outer = b.load(info.addr);
                        b.load_typed(outer, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    } else {
                        let zero = b.const_i64(0);
                        b.gep(info.addr, vec![zero], IrType::Int(IntWidth::I8))
                    };
                    Some((ptr, *n))
                }
                CharKind::Deferred | CharKind::None => None,
            }
        }
        Expr::StringLiteral { value, .. } => {
            let ptr = b.const_string(value.as_bytes());
            Some((ptr, value.len() as i64))
        }
        _ => None,
    }
}

fn char_addr_and_runtime_len(
    b: &mut FuncBuilder,
    arg_spanned: &crate::ast::expr::SpannedExpr,
    locals: &HashMap<String, LocalInfo>,
) -> Option<(ValueId, ValueId)> {
    use crate::ast::expr::Expr;
    match &arg_spanned.node {
        Expr::Name { name } => {
            let info = locals.get(&name.to_lowercase())?;
            match &info.char_kind {
                CharKind::Fixed(n) => {
                    if !info.dims.is_empty() {
                        return None;
                    }
                    let ptr = if info.by_ref {
                        let outer = b.load(info.addr);
                        b.load_typed(outer, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    } else {
                        let zero = b.const_i64(0);
                        b.gep(info.addr, vec![zero], IrType::Int(IntWidth::I8))
                    };
                    let len = b.const_i64(*n);
                    Some((ptr, len))
                }
                CharKind::Deferred => {
                    let ptr = b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                    let eight = b.const_i64(8);
                    let len_ptr = b.gep(info.addr, vec![eight], IrType::Int(IntWidth::I8));
                    let len = b.load_typed(len_ptr, IrType::Int(IntWidth::I64));
                    Some((ptr, len))
                }
                CharKind::None => {
                    if info.by_ref
                        && matches!(
                            info.ty,
                            IrType::Ptr(ref inner) if matches!(inner.as_ref(), IrType::Int(IntWidth::I8))
                        )
                    {
                        let outer = b.load(info.addr);
                        let ptr = b.load_typed(outer, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        let len = b.call(
                            FuncRef::External("afs_c_strlen".into()),
                            vec![ptr],
                            IrType::Int(IntWidth::I64),
                        );
                        Some((ptr, len))
                    } else {
                        None
                    }
                }
            }
        }
        Expr::StringLiteral { value, .. } => {
            let ptr = b.const_string(value.as_bytes());
            let len = b.const_i64(value.len() as i64);
            Some((ptr, len))
        }
        _ => None,
    }
}

/// Lower character intrinsic functions (LEN, LEN_TRIM, ICHAR, CHAR, INDEX, SCAN, VERIFY,
/// ADJUSTL, ADJUSTR, TRIM). These need access to `locals` (for CharKind info) and the
/// original un-lowered argument expressions, so they cannot go through `lower_intrinsic`.
/// Returns Some(ValueId) if recognized, None otherwise.
fn lower_char_intrinsic(
    b: &mut FuncBuilder,
    name: &str,
    args: &[crate::ast::expr::Argument],
    locals: &HashMap<String, LocalInfo>,
    st: &SymbolTable,
) -> Option<ValueId> {
    use crate::ast::expr::SectionSubscript;

    // Extract the SpannedExpr from argument i.
    let arg_spanned = |i: usize| -> Option<&crate::ast::expr::SpannedExpr> {
        args.get(i).and_then(|a| {
            if let SectionSubscript::Element(e) = &a.value { Some(e) } else { None }
        })
    };

    match name {
        "len" => {
            let (_, len) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            Some(len)
        }
        "len_trim" => {
            let (ptr, len_val) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            Some(b.call(FuncRef::External("afs_len_trim".into()),
                vec![ptr, len_val], IrType::Int(IntWidth::I64)))
        }
        "ichar" => {
            let (ptr, _) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            let byte = b.load_typed(ptr, IrType::Int(IntWidth::I8));
            Some(b.call(FuncRef::External("afs_ichar".into()),
                vec![byte], IrType::Int(IntWidth::I32)))
        }
        "char" => {
            let int_arg = args.first().and_then(|a| {
                if let SectionSubscript::Element(e) = &a.value {
                    Some(lower_expr(b, locals, e, st))
                } else { None }
            })?;
            let i32_arg = match b.func().value_type(int_arg) {
                Some(IrType::Int(IntWidth::I64)) => b.int_trunc(int_arg, IntWidth::I32),
                _ => int_arg,
            };
            let byte_val = b.call(FuncRef::External("afs_char".into()),
                vec![i32_arg], IrType::Int(IntWidth::I8));
            // Allocate a 1-byte buffer and store through a byte-level GEP to avoid
            // the Ptr<[i8 x 1]> vs Ptr<i8> store-type mismatch.
            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 1));
            let zero = b.const_i64(0);
            let byte_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
            b.store(byte_val, byte_ptr);
            Some(buf)
        }
        "index" => {
            let (hay_ptr, hay_len_val) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            let (needle_ptr, needle_len_val) = char_addr_and_runtime_len(b, arg_spanned(1)?, locals)?;
            let back_val = arg_spanned(2)
                .map(|e| lower_expr(b, locals, e, st))
                .unwrap_or_else(|| b.const_i32(0));
            Some(b.call(FuncRef::External("afs_index".into()),
                vec![hay_ptr, hay_len_val, needle_ptr, needle_len_val, back_val],
                IrType::Int(IntWidth::I64)))
        }
        "scan" => {
            let (src_ptr, src_len_val) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            let (set_ptr, set_len_val) = char_addr_and_runtime_len(b, arg_spanned(1)?, locals)?;
            let back_val = arg_spanned(2)
                .map(|e| lower_expr(b, locals, e, st))
                .unwrap_or_else(|| b.const_i32(0));
            Some(b.call(FuncRef::External("afs_scan".into()),
                vec![src_ptr, src_len_val, set_ptr, set_len_val, back_val],
                IrType::Int(IntWidth::I64)))
        }
        "verify" => {
            let (src_ptr, src_len_val) = char_addr_and_runtime_len(b, arg_spanned(0)?, locals)?;
            let (set_ptr, set_len_val) = char_addr_and_runtime_len(b, arg_spanned(1)?, locals)?;
            let back_val = arg_spanned(2)
                .map(|e| lower_expr(b, locals, e, st))
                .unwrap_or_else(|| b.const_i32(0));
            Some(b.call(FuncRef::External("afs_verify".into()),
                vec![src_ptr, src_len_val, set_ptr, set_len_val, back_val],
                IrType::Int(IntWidth::I64)))
        }
        "adjustl" => {
            let (src_ptr, src_len) = char_addr_and_len(b, arg_spanned(0)?, locals)?;
            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), src_len as u64));
            let len_val = b.const_i64(src_len);
            b.call(FuncRef::External("afs_adjustl".into()),
                vec![buf, src_ptr, len_val], IrType::Void);
            Some(buf)
        }
        "adjustr" => {
            let (src_ptr, src_len) = char_addr_and_len(b, arg_spanned(0)?, locals)?;
            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), src_len as u64));
            let len_val = b.const_i64(src_len);
            b.call(FuncRef::External("afs_adjustr".into()),
                vec![buf, src_ptr, len_val], IrType::Void);
            Some(buf)
        }
        "trim" => {
            // TRIM(s): returns character with trailing blanks removed.
            // Allocate buffer of declared length, memcpy source, return buffer pointer.
            // The actual printed length is discovered by len_trim at the call site.
            let (src_ptr, src_len) = char_addr_and_len(b, arg_spanned(0)?, locals)?;
            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), src_len as u64));
            let len_val = b.const_i64(src_len);
            b.call(FuncRef::External("memcpy".into()),
                vec![buf, src_ptr, len_val], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            Some(buf)
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
                } else if is_complex_ty(&ty) {
                    // real(z) extracts the real component of a complex number.
                    // Complex values live as ptr<[f32/f64 x 2]>; load element 0.
                    let fw = complex_float_width(&ty);
                    let zero = b.const_i64(0);
                    let re_ptr = b.gep(*arg, vec![zero], IrType::Int(IntWidth::I8));
                    Some(b.load_typed(re_ptr, IrType::Float(fw)))
                } else {
                    Some(*arg)
                }
            } else { None }
        }
        "aimag" | "dimag" => {
            // aimag(z) extracts the imaginary component of a complex number.
            // Complex values live as ptr<[f32/f64 x 2]>; load element 1 at
            // byte offset 4 (f32) or 8 (f64).
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if is_complex_ty(&ty) {
                    let fw = complex_float_width(&ty);
                    let offset = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                    let im_ptr = b.gep(*arg, vec![offset], IrType::Int(IntWidth::I8));
                    Some(b.load_typed(im_ptr, IrType::Float(fw)))
                } else {
                    None
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
                    IrType::Int(IntWidth::I128) => 38,
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
                    IrType::Int(IntWidth::I128) => 127,
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
                    IrType::Int(IntWidth::I128) => 128,
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
                    IrType::Int(IntWidth::I128) => 16,
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
                    IrType::Int(IntWidth::I128) => 16,
                    IrType::Ptr(_) => 8, // pointers are 8 bytes on ARM64
                    // Arrays use element size * count, but we don't have shape info here.
                    // For now, return element size. Proper impl needs descriptor access.
                    IrType::Array(elem, count) => {
                        let elem_size = ir_scalar_byte_size(elem.as_ref());
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
/// Determine the CharKind for a dummy argument from its declaration.
///
/// Returns `CharKind::Fixed(n)` if the declaration is
/// `character(len=n)`, `CharKind::None` otherwise. Assumed-length
/// dummies (`character(len=*)`) currently return `CharKind::None`
/// because the hidden-length ABI parameter that would supply the
/// runtime length is not yet implemented.
fn arg_char_kind_from_decls(arg_name: &str, decls: &[crate::ast::decl::SpannedDecl]) -> CharKind {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    match type_spec {
                        TypeSpec::Character(Some(sel)) => {
                            match &sel.len {
                                Some(crate::ast::decl::LenSpec::Expr(e)) => {
                                    if let Some(n) = eval_const_int_in_scope(e, &HashMap::new()) {
                                        return CharKind::Fixed(n);
                                    }
                                }
                                _ => {}
                            }
                        }
                        TypeSpec::Character(None) => return CharKind::Fixed(1),
                        _ => {}
                    }
                    return CharKind::None;
                }
            }
        }
    }
    CharKind::None
}

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

fn arg_dims_from_decls(
    arg_name: &str,
    decls: &[crate::ast::decl::SpannedDecl],
    visible_param_consts: &HashMap<String, ConstScalar>,
) -> Vec<(i64, i64)> {
    let key = arg_name.to_lowercase();
    let param_consts = collect_decl_param_consts_with_host(decls, visible_param_consts);
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let crate::ast::decl::Attribute::Dimension(specs) = a {
                    Some(specs)
                } else {
                    None
                }
            });
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    let array_spec = entity.array_spec.as_ref().or(attr_dims);
                    return array_spec
                        .map(|specs| extract_array_dims(specs, &param_consts))
                        .unwrap_or_default();
                }
            }
        }
    }
    Vec::new()
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

fn callee_return_ir_type(st: &SymbolTable, callee_name: &str) -> Option<IrType> {
    use crate::sema::symtab::ScopeKind;

    let key = callee_name.to_lowercase();
    if let Some(type_info) = st
        .scopes
        .iter()
        .find_map(|scope| scope.symbols.get(&key))
        .and_then(|sym| sym.type_info.as_ref())
    {
        return Some(type_info_to_ir_type(type_info));
    }

    let callee_scope = st.scopes.iter().find(|scope| {
        matches!(&scope.kind, ScopeKind::Function(name) if name.to_lowercase() == key)
    })?;

    let mut result_type = None;
    for sym in callee_scope.symbols.values() {
        if callee_scope.arg_order.iter().any(|arg| arg == &sym.name.to_lowercase()) {
            continue;
        }
        if let Some(type_info) = sym.type_info.as_ref() {
            if result_type.is_some() {
                return None;
            }
            result_type = Some(type_info_to_ir_type(type_info));
        }
    }
    result_type
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

fn arg_is_fortran_noalias(arg_name: &str, decls: &[crate::ast::decl::SpannedDecl]) -> bool {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    return !attrs.iter().any(|attr| {
                        matches!(
                            attr,
                            crate::ast::decl::Attribute::Pointer
                                | crate::ast::decl::Attribute::Target
                                | crate::ast::decl::Attribute::Value
                        )
                    });
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
                    CharKind::Fixed(_) | CharKind::Deferred => {
                        // Delegate to char_addr_and_runtime_len which
                        // correctly handles both local buffers (GEP
                        // to element 0) and by_ref dummies (double
                        // load through the wrapper alloca).  The
                        // previous inline path returned info.addr
                        // raw for Fixed, which is wrong for dummies.
                        if let Some((ptr, len)) = char_addr_and_runtime_len(b, expr, locals) {
                            (ptr, len)
                        } else {
                            let val = lower_expr(b, locals, expr, st);
                            let zero = b.const_i64(0);
                            (val, zero)
                        }
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
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                let first_char_arg = args.first().and_then(|a| {
                    if let crate::ast::expr::SectionSubscript::Element(e) = &a.value { Some(e) } else { None }
                });
                match key.as_str() {
                    "trim" => {
                        if let Some(arg) = first_char_arg {
                            if let Some((src_ptr, declared_len)) = char_addr_and_len(b, arg, locals) {
                                let len_val = b.const_i64(declared_len);
                                let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), declared_len as u64));
                                b.call(FuncRef::External("memcpy".into()),
                                    vec![buf, src_ptr, len_val],
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                // Compute the trimmed length at runtime.
                                let trimmed_len = b.call(FuncRef::External("afs_len_trim".into()),
                                    vec![src_ptr, len_val], IrType::Int(IntWidth::I64));
                                return (buf, trimmed_len);
                            }
                        }
                    }
                    "adjustl" => {
                        if let Some(arg) = first_char_arg {
                            if let Some((src_ptr, declared_len)) = char_addr_and_len(b, arg, locals) {
                                let len_val = b.const_i64(declared_len);
                                let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), declared_len as u64));
                                b.call(FuncRef::External("afs_adjustl".into()),
                                    vec![buf, src_ptr, len_val], IrType::Void);
                                return (buf, len_val);
                            }
                        }
                    }
                    "adjustr" => {
                        if let Some(arg) = first_char_arg {
                            if let Some((src_ptr, declared_len)) = char_addr_and_len(b, arg, locals) {
                                let len_val = b.const_i64(declared_len);
                                let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), declared_len as u64));
                                b.call(FuncRef::External("afs_adjustr".into()),
                                    vec![buf, src_ptr, len_val], IrType::Void);
                                return (buf, len_val);
                            }
                        }
                    }
                    "char" => {
                        // CHAR(i) → 1-byte buffer.
                        if let Some(arg) = first_char_arg {
                            let int_val = lower_expr(b, locals, arg, st);
                            let i32_val = match b.func().value_type(int_val) {
                                Some(IrType::Int(IntWidth::I64)) => b.int_trunc(int_val, IntWidth::I32),
                                _ => int_val,
                            };
                            let byte_val = b.call(FuncRef::External("afs_char".into()),
                                vec![i32_val], IrType::Int(IntWidth::I8));
                            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 1));
                            let zero = b.const_i64(0);
                            let byte_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
                            b.store(byte_val, byte_ptr);
                            let one = b.const_i64(1);
                            return (buf, one);
                        }
                    }
                    _ => {}
                }

                // Substring designator on a character variable: `s(lo:hi)`.
                // Parser produces this as FunctionCall { callee: Name(s),
                // args: [Range(lo, hi)] } — without this case we fall
                // through to lower_expr and emit an external call to
                // `_s`, which fails at link time.
                if args.len() == 1 {
                    if let crate::ast::expr::SectionSubscript::Range { start, end, stride: _ } = &args[0].value {
                        if locals.get(&key).map(|i| i.char_kind != CharKind::None && i.dims.is_empty()).unwrap_or(false) {
                            if let Some((base_ptr, base_len)) = char_addr_and_runtime_len(b, callee, locals) {
                                return lower_substring(b, locals, st, base_ptr, base_len, start.as_ref(), end.as_ref());
                            }
                        }
                    }
                }
            }
            // Nested FunctionCall: arr(i)(lo:hi) — substring of a
            // character array element. The outer callee is itself a
            // FunctionCall (the element access) with a Range arg.
            if let Expr::FunctionCall { callee: inner_callee, args: inner_args } = &callee.node {
                if let Expr::Name { name: arr_name } = &inner_callee.node {
                    let akey = arr_name.to_lowercase();
                    if let Some(info) = locals.get(&akey) {
                        if matches!(info.char_kind, CharKind::Fixed(_))
                            && (!info.dims.is_empty() || info.allocatable)
                            && args.len() == 1
                        {
                            if let crate::ast::expr::SectionSubscript::Range { ref start, ref end, .. } = args[0].value {
                                // Get the char-array element's string pointer and length.
                                let idx64 = if inner_args.len() == 1 {
                                    if let crate::ast::expr::SectionSubscript::Element(idx_expr) = &inner_args[0].value {
                                        let idx = lower_expr(b, locals, idx_expr, st);
                                        let idx_wide = match b.func().value_type(idx) {
                                            Some(IrType::Int(IntWidth::I64)) => idx,
                                            _ => b.int_extend(idx, IntWidth::I64, true),
                                        };
                                        let one = b.const_i64(1);
                                        b.isub(idx_wide, one)
                                    } else { b.const_i64(0) }
                                } else { b.const_i64(0) };
                                let base = array_base_addr(b, info);
                                let elem_slot = b.gep(base, vec![idx64], info.ty.clone());
                                let elem_ptr = b.load_typed(elem_slot, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                let elem_len = match info.char_kind {
                                    CharKind::Fixed(n) => b.const_i64(n),
                                    _ => b.const_i64(0),
                                };
                                return lower_substring(b, locals, st, elem_ptr, elem_len, start.as_ref(), end.as_ref());
                            }
                        }
                    }
                }
            }
            let val = lower_expr(b, locals, expr, st);
            let len = b.const_i64(string_literal_len(expr));
            (val, len)
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

/// True if `ty` is the complex representation: `[f32/f64 x 2]` or `ptr<[f32/f64 x 2]>`.
/// Complex allocas have pointer type in the IR; the underlying element type is the array.
fn is_complex_ty(ty: &IrType) -> bool {
    match ty {
        IrType::Array(ref e, 2) => matches!(e.as_ref(), IrType::Float(_)),
        IrType::Ptr(ref inner) => {
            matches!(inner.as_ref(), IrType::Array(ref e, 2) if matches!(e.as_ref(), IrType::Float(_)))
        }
        _ => false,
    }
}

/// Float width of a complex type, whether `[f32/f64 x 2]` or `ptr<[f32/f64 x 2]>`.
fn complex_float_width(ty: &IrType) -> FloatWidth {
    let elem = match ty {
        IrType::Array(ref e, 2) => e.as_ref(),
        IrType::Ptr(ref inner) => match inner.as_ref() {
            IrType::Array(ref e, 2) => e.as_ref(),
            _ => return FloatWidth::F32,
        },
        _ => return FloatWidth::F32,
    };
    match elem {
        IrType::Float(FloatWidth::F64) => FloatWidth::F64,
        _ => FloatWidth::F32,
    }
}

/// Byte size of a complex value stored as `[f32 x 2]` (8) or `[f64 x 2]` (16).
fn complex_byte_size(ty: &IrType) -> i64 {
    if complex_float_width(ty) == FloatWidth::F64 { 16 } else { 8 }
}

/// Insert implicit deallocation calls for all local allocatable variables.
/// Uses a dummy STAT variable so already-deallocated arrays don't abort.
///
/// Iterates locals in alphabetical order by name to make the emitted
/// IR (and therefore the assembly) deterministic across runs. The
/// previous version walked `locals.values()` directly, picking up the
/// HashMap's randomized iteration order — surfaced as non-reproducible
/// builds for any function with multiple allocatable locals.
/// When `skip_addr` is `Some(addr)`, skip deallocation for any local whose
/// `info.addr` matches. Used to preserve sret result ownership: the result
/// variable of an allocatable-returning function is allocated inside the
/// callee but ownership is transferred to the caller — the callee must
/// NOT free it. Audit6 BLOCKING-1.
fn insert_implicit_dealloc(b: &mut FuncBuilder, locals: &HashMap<String, LocalInfo>, type_layouts: &crate::sema::type_layout::TypeLayoutRegistry, skip_addr: Option<ValueId>) {
    // Audit Med-2: only allocate the stat_addr scratch slot if we
    // actually need it for an `afs_deallocate_array` call. Without
    // this guard every function (even one with no allocatables)
    // got a zombie i32 alloca right before its ret, bloating the
    // frame and the IR — and DCE couldn't drop it because allocas
    // are classified as side-effecting.
    let needs_dealloc = locals.values()
        .any(|info| info.allocatable || info.char_kind == CharKind::Deferred);
    let needs_stat = locals.values().any(|info| info.allocatable);
    if !needs_dealloc && !locals.values().any(|info|
        !info.by_ref && info.derived_type.as_ref()
            .and_then(|tn| type_layouts.get(tn))
            .is_some_and(|l| !l.final_procs.is_empty()))
    {
        return;
    }

    let stat_addr = if needs_stat {
        Some(b.alloca(IrType::Int(IntWidth::I32)))
    } else {
        None
    };
    let mut sorted: Vec<(&String, &LocalInfo)> = locals.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (_name, info) in sorted {
        // Skip caller-owned allocatables (sret result variables).
        if skip_addr == Some(info.addr) { continue; }
        // Skip pointers: a POINTER variable does not own its target.
        // Its slot may look allocatable-shaped (pointer-to-array uses
        // the 384-byte descriptor layout) but the base_addr belongs
        // to whatever TARGET the pointer is associated with — freeing
        // it through the pointer would double-free or free stack
        // storage.  F2018 19.5 distinguishes POINTER deallocation
        // (explicit DEALLOCATE(p)) from scope exit.
        if info.is_pointer { continue; }
        if info.char_kind == CharKind::Deferred {
            b.call(
                FuncRef::External("afs_dealloc_string".into()),
                vec![info.addr],
                IrType::Void,
            );
        } else if info.allocatable {
            b.call(
                FuncRef::External("afs_deallocate_array".into()),
                vec![info.addr, stat_addr.unwrap()],
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
/// Pre-scan a body of statements and create one IR basic block per
/// Fortran statement label. Must be called before `lower_stmts` so
/// that both forward and backward `GOTO` targets can branch to an
/// already-existing block.
fn collect_label_blocks(b: &mut FuncBuilder, stmts: &[SpannedStmt], out: &mut HashMap<u64, BlockId>) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::Labeled { label, stmt: inner } => {
                let bb = b.create_block(&format!("label_{}", label));
                out.entry(*label).or_insert(bb);
                // Recurse into the inner statement (e.g., a DO or IF block with labels inside).
                collect_label_blocks(b, std::slice::from_ref(inner.as_ref()), out);
            }
            Stmt::Continue { label: Some(lbl) } => {
                let bb = b.create_block(&format!("label_{}", lbl));
                out.entry(*lbl).or_insert(bb);
            }
            Stmt::IfConstruct { then_body, else_ifs, else_body, .. } => {
                collect_label_blocks(b, then_body, out);
                for (_, body) in else_ifs { collect_label_blocks(b, body, out); }
                if let Some(body) = else_body { collect_label_blocks(b, body, out); }
            }
            Stmt::IfStmt { action, .. } => {
                collect_label_blocks(b, std::slice::from_ref(action.as_ref()), out);
            }
            Stmt::DoLoop { body, .. } | Stmt::DoWhile { body, .. } | Stmt::DoConcurrent { body, .. } => {
                collect_label_blocks(b, body, out);
            }
            _ => {}
        }
    }
}

fn lower_stmts(b: &mut FuncBuilder, ctx: &mut LowerCtx, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        // Labeled statements and labeled CONTINUEs create new basic blocks; they must be
        // processed even after a branch/goto terminates the current block. All other dead
        // code (statements after a terminator in an unlabeled position) is skipped.
        let is_label_creating = matches!(&stmt.node,
            Stmt::Labeled { .. } | Stmt::Continue { label: Some(_) });
        if !is_label_creating && b.func().block(b.current_block()).terminator.is_some() {
            continue; // dead code — but keep looping so we can find the next label
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
                                if !info.dims.is_empty() || info.allocatable {
                                    if try_lower_elemental_array_assign(b, ctx, name, &info, value) {
                                        return;
                                    }
                                    if let Expr::FunctionCall { callee, args: call_args } = &value.node {
                                        if let Expr::Name { name: callee_name } = &callee.node {
                                            let callee_key = callee_name.to_lowercase();
                                            if ctx.alloc_return_funcs.contains(&callee_key) {
                                                // Audit6 BLOCKING-1: sret call — pass info.addr as
                                                // the hidden first arg so the function writes its
                                                // result directly into the destination descriptor.
                                                // No temp descriptor or afs_assign_allocatable needed.
                                                let ref_args: Vec<ValueId> = call_args.iter().map(|a| {
                                                    match &a.value {
                                                        crate::ast::expr::SectionSubscript::Element(e) =>
                                                            lower_arg_by_ref(b, &ctx.locals, e, ctx.st),
                                                        _ => b.const_i32(0),
                                                    }
                                                }).collect();
                                                let mut all_args = vec![info.addr];
                                                all_args.extend(ref_args);
                                                b.call(FuncRef::External(callee_name.clone()), all_args, IrType::Void);
                                            } else {
                                                // Non-sret: function returns a temp descriptor.
                                                let src_desc = lower_expr_ctx_tl(b, ctx, value);
                                                b.call(FuncRef::External("afs_assign_allocatable".into()),
                                                    vec![info.addr, src_desc], IrType::Void);
                                                let stat = b.alloca(IrType::Int(IntWidth::I32));
                                                b.call(FuncRef::External("afs_deallocate_array".into()),
                                                    vec![src_desc, stat], IrType::Void);
                                            }
                                        } else {
                                            // Indirect callee: fall back to assign path.
                                            let src_desc = lower_expr_ctx_tl(b, ctx, value);
                                            b.call(FuncRef::External("afs_assign_allocatable".into()),
                                                vec![info.addr, src_desc], IrType::Void);
                                            let stat = b.alloca(IrType::Int(IntWidth::I32));
                                            b.call(FuncRef::External("afs_deallocate_array".into()),
                                                vec![src_desc, stat], IrType::Void);
                                        }
                                    } else {
                                        lower_array_assign(b, ctx, name, &info, value);
                                    }
                                } else if info.derived_type.is_some() {
                                    let val = lower_expr_ctx_tl(b, ctx, value);
                                    let size = if let Some(ref tn) = info.derived_type {
                                        ctx.type_layouts.get(tn).map(|l| l.size).unwrap_or(8)
                                    } else { 8 };
                                    let size_val = b.const_i64(size as i64);
                                    b.call(FuncRef::External("memcpy".into()),
                                        vec![info.addr, val, size_val],
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                } else if info.is_pointer {
                                    // Plain `=` on a POINTER dereferences:
                                    // load the target address out of the
                                    // pointer slot, then store through it.
                                    let val = lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    let tgt = b.load_typed(info.addr, IrType::Ptr(Box::new(info.ty.clone())));
                                    b.store(coerced, tgt);
                                } else if is_complex_ty(&info.ty) {
                                    // Complex assignment: RHS returns a ptr to [f32/f64 x 2] buffer.
                                    // Memcpy the 8 or 16 bytes into the destination slot.
                                    let src = lower_expr_ctx_tl(b, ctx, value);
                                    let bytes = complex_byte_size(&info.ty);
                                    let sz = b.const_i64(bytes);
                                    if info.by_ref {
                                        let dst = b.load(info.addr);
                                        b.call(FuncRef::External("memcpy".into()),
                                            vec![dst, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                    } else {
                                        b.call(FuncRef::External("memcpy".into()),
                                            vec![info.addr, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                    }
                                } else if info.by_ref {
                                    let val = lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    let ptr = b.load(info.addr);
                                    b.store(coerced, ptr);
                                } else {
                                    let val = lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    b.store(coerced, info.addr);
                                }
                            }
                        }
                    }
                }
                Expr::FunctionCall { callee, args } => {
                    if let Expr::Name { name } = &callee.node {
                        let akey = name.to_lowercase();
                        if let Some(info) = ctx.locals.get(&akey).cloned() {
                            // Substring LHS: s(lo:hi) = rhs where s is a
                            // scalar character.  Compute the target substring
                            // pointer+length, get the RHS as (ptr, len), and
                            // call afs_assign_char_fixed to do the bounded
                            // copy with space-padding.
                            if info.char_kind != CharKind::None
                                && info.dims.is_empty()
                                && args.len() == 1
                                && matches!(args[0].value, crate::ast::expr::SectionSubscript::Range { .. })
                            {
                                if let crate::ast::expr::SectionSubscript::Range { ref start, ref end, .. } = args[0].value {
                                    if let Some((base_ptr, base_len)) = char_addr_and_runtime_len(b, callee, &ctx.locals) {
                                        let (dest_ptr, dest_len) = lower_substring(
                                            b, &ctx.locals, ctx.st,
                                            base_ptr, base_len,
                                            start.as_ref(), end.as_ref(),
                                        );
                                        let (src_ptr, src_len) = lower_string_expr(b, &ctx.locals, value, ctx.st);
                                        b.call(
                                            FuncRef::External("afs_assign_char_fixed".into()),
                                            vec![dest_ptr, dest_len, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                    }
                                }
                            } else if !info.dims.is_empty() || info.allocatable {
                                // Array element assignment: a(i) = val
                                if matches!(info.char_kind, CharKind::Fixed(_)) {
                                    lower_char_array_store(b, &ctx.locals, &info, args, value, ctx.st);
                                } else {
                                    let arr_val = lower_expr_ctx(b, ctx, value);
                                    lower_array_store(b, &ctx.locals, &info, args, arr_val, ctx.st);
                                }
                            }
                        }
                    }
                }
                Expr::ComponentAccess { base, component } => {
                    // x%field = val (supports chained: x%a%b = val).
                    if let Some((base_addr, type_name)) = resolve_component_base(b, &ctx.locals, base, ctx.type_layouts) {
                        if let Some(layout) = ctx.type_layouts.get(&type_name) {
                            if let Some(field) = layout.field(component) {
                                let val = lower_expr_ctx_tl(b, ctx, value);
                                let coerced = coerce_to_type(
                                    b,
                                    val,
                                    &type_info_to_ir_type(&field.type_info),
                                );
                                let offset = b.const_i64(field.offset as i64);
                                let field_ptr = b.gep(base_addr, vec![offset],
                                    IrType::Int(IntWidth::I8));
                                b.store(coerced, field_ptr);
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

            if let Some(ctrl) = controls.first() {
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_write_items(b, ctx, items, buf_ptr, buf_len);
                    } else {
                        let (fmt_ptr, fmt_len) = lower_string_expr(b, &ctx.locals, &fmt_control.unwrap().value, ctx.st);
                        b.call(
                            FuncRef::External("afs_fmt_begin_internal".into()),
                            vec![buf_ptr, buf_len, fmt_ptr, fmt_len],
                            IrType::Void,
                        );
                        for item in items {
                            lower_fmt_push(b, ctx, item);
                        }
                        let adv = b.const_i32(if advance { 1 } else { 0 });
                        b.call(FuncRef::External("afs_fmt_end".into()), vec![adv], IrType::Void);
                    }
                    return;
                }
            }

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
                    let mut arg_vals: Vec<ValueId> = args.iter().map(|a| {
                        match &a.value {
                            crate::ast::expr::SectionSubscript::Element(e) => {
                                lower_arg_by_ref(b, &ctx.locals, e, ctx.st)
                            }
                            _ => b.const_i32(0),
                        }
                    }).collect();
                    if let Some(desc_mask) = ctx.descriptor_params.get(&key) {
                        for (i, a) in args.iter().enumerate() {
                            if !desc_mask.get(i).copied().unwrap_or(false) {
                                continue;
                            }
                            arg_vals[i] = match &a.value {
                                crate::ast::expr::SectionSubscript::Element(e) => {
                                    lower_arg_descriptor(b, &ctx.locals, e, ctx.st)
                                }
                                _ => b.const_i64(0),
                            };
                        }
                    }
                    // If the callee has more parameters than provided args, and the
                    // trailing ones are OPTIONAL, pass null pointers so PRESENT() works.
                    if let Some(opt_flags) = ctx.optional_params.get(&key) {
                        for flag in opt_flags.iter().skip(arg_vals.len()) {
                            if *flag {
                                arg_vals.push(b.const_i64(0)); // null → absent
                            }
                        }
                    }
                    let func_ref = ctx
                        .internal_funcs
                        .get(&key)
                        .copied()
                        .map(FuncRef::Internal)
                        .unwrap_or_else(|| FuncRef::External(name.clone()));
                    b.call(func_ref, arg_vals, IrType::Void);
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
            lower_do_loop(b, ctx, DoLoopFields { name, var, start, end, step, body, concurrent: false });
        }

        Stmt::DoConcurrent { name, controls, mask, body, locality: _, .. } => {
            lower_do_concurrent(b, ctx, name, controls, mask.as_ref(), body, stmt.span);
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

        Stmt::WhereConstruct { mask, body, elsewhere, .. } => {
            // WHERE(mask) body [ELSEWHERE body] END WHERE
            // Collect ALL array names referenced in mask or body.
            let mut array_names: Vec<String> = Vec::new();
            collect_array_names(mask, &ctx.locals, &mut array_names);
            for s in body {
                collect_array_names_stmt(s, &ctx.locals, &mut array_names);
            }

            if array_names.is_empty() {
                // No arrays — fall back to scalar IF-THEN-ELSE.
                let cond = lower_expr_tl(b, &ctx.locals, mask, ctx.st, ctx.type_layouts);
                let bb_then = b.create_block("where_then");
                let bb_else = if !elsewhere.is_empty() {
                    Some(b.create_block("where_else"))
                } else { None };
                let bb_end = b.create_block("where_end");
                b.cond_branch(cond, bb_then, vec![], bb_else.unwrap_or(bb_end), vec![]);

                b.set_block(bb_then);
                lower_stmts(b, ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }
                if let Some(bb_e) = bb_else {
                    b.set_block(bb_e);
                    if let Some((_m, else_body)) = elsewhere.first() {
                        lower_stmts(b, ctx, else_body);
                    }
                    if b.func().block(b.current_block()).terminator.is_none() {
                        b.branch(bb_end, vec![]);
                    }
                }
                b.set_block(bb_end);
                return;
            }

            // Array-level WHERE: iterate over elements.
            // Use the first array to determine the iteration count. For
            // stack arrays `info.addr` is the raw element buffer — calling
            // afs_array_size on that would read garbage out of the rank
            // slot. array_total_elems_value picks the right source: it
            // materialises a descriptor query for descriptor-backed locals
            // and folds dims to a constant for explicit-shape stack arrays.
            let first_arr_name = &array_names[0];
            let first_arr = ctx.locals.get(first_arr_name).cloned().expect("array must exist");
            let n = array_total_elems_value(b, &first_arr);

            // Get base addresses for all arrays (loaded once outside the loop).
            let mut array_bases: HashMap<String, ValueId> = HashMap::new();
            for arr_name in &array_names {
                if let Some(info) = ctx.locals.get(arr_name) {
                    let base = if info.allocatable {
                        b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    } else { info.addr };
                    array_bases.insert(arr_name.clone(), base);
                }
            }

            let i_addr = b.alloca(IrType::Int(IntWidth::I64));
            let i_zero = b.const_i64(0);
            b.store(i_zero, i_addr);

            let bb_check = b.create_block("where_check");
            let bb_body = b.create_block("where_body");
            let bb_exit = b.create_block("where_exit");
            b.branch(bb_check, vec![]);

            b.set_block(bb_check);
            let i = b.load(i_addr);
            let done = b.icmp(CmpOp::Ge, i, n);
            b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

            b.set_block(bb_body);
            let i_val = b.load(i_addr);

            // Substitute each array variable with a scalar local bound to element i.
            // Save original locals for restoration.
            let mut saved_locals: Vec<(String, Option<LocalInfo>)> = Vec::new();
            for arr_name in &array_names {
                saved_locals.push((arr_name.clone(), ctx.locals.get(arr_name).cloned()));
                if let Some(orig_info) = ctx.locals.get(arr_name).cloned() {
                    let base = *array_bases.get(arr_name).unwrap();
                    // Compute element address: base + i * elem_bytes.
                    let elem_bytes_val = match &orig_info.ty {
                        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => b.const_i64(8),
                        IrType::Int(IntWidth::I16) => b.const_i64(2),
                        IrType::Int(IntWidth::I8) => b.const_i64(1),
                        _ => b.const_i64(4),
                    };
                    let byte_off = b.imul(i_val, elem_bytes_val);
                    let elem_ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
                    // Replace the local with a scalar pointing to this element.
                    ctx.locals.insert(arr_name.clone(), LocalInfo {
                        addr: elem_ptr,
                        ty: orig_info.ty.clone(),
                        dims: vec![],
                        allocatable: false,
                        descriptor_arg: false,
                        by_ref: false,
                        char_kind: CharKind::None,
                        derived_type: None, inline_const: None, is_pointer: false,
                    });
                }
            }

            // Evaluate mask with element-level bindings.
            let cond = lower_expr_tl(b, &ctx.locals, mask, ctx.st, ctx.type_layouts);

            let bb_then = b.create_block("where_then");
            let bb_else = b.create_block("where_else");
            let bb_incr = b.create_block("where_incr");
            b.cond_branch(cond, bb_then, vec![], bb_else, vec![]);

            b.set_block(bb_then);
            lower_stmts(b, ctx, body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_incr, vec![]);
            }

            b.set_block(bb_else);
            if let Some((_else_mask, else_body)) = elsewhere.first() {
                lower_stmts(b, ctx, else_body);
            }
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_incr, vec![]);
            }

            b.set_block(bb_incr);
            // Restore original locals.
            for (name, orig) in saved_locals {
                if let Some(info) = orig {
                    ctx.locals.insert(name, info);
                } else {
                    ctx.locals.remove(&name);
                }
            }

            let i_cur = b.load(i_addr);
            let one = b.const_i64(1);
            let next = b.iadd(i_cur, one);
            b.store(next, i_addr);
            b.branch(bb_check, vec![]);

            b.set_block(bb_exit);
        }

        Stmt::WhereStmt { mask, stmt } => {
            // Single-line WHERE: where (cond) assignment
            let cond = lower_expr_tl(b, &ctx.locals, mask, ctx.st, ctx.type_layouts);
            let bb_then = b.create_block("where_stmt");
            let bb_end = b.create_block("where_stmt_end");
            b.cond_branch(cond, bb_then, vec![], bb_end, vec![]);
            b.set_block(bb_then);
            lower_stmt(b, ctx, stmt);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }
            b.set_block(bb_end);
        }

        Stmt::ForallConstruct { specs, mask, body, .. } => {
            // FORALL: nest loops. The body goes inside the innermost loop.
            // Build the body statements including optional mask as a closure-like pattern.
            // The innermost loop gets the real body; outer loops wrap it.
            lower_forall_nested(b, ctx, specs, mask.as_ref(), body);
        }

        Stmt::ForallStmt { specs, mask, stmt } => {
            let body_vec = vec![(**stmt).clone()];
            lower_forall_nested(b, ctx, specs, mask.as_ref(), &body_vec);
        }

        Stmt::SelectType { selector, guards, assoc_name: _, .. } => {
            // SELECT TYPE: compare the type tag of a polymorphic variable.
            // For now, support basic pattern where selector is a local derived type
            // variable and TYPE IS guards match by type tag.
            let bb_end = b.create_block("select_type_end");

            // Get the selector's type tag. For non-polymorphic variables,
            // we know the static type and can match directly.
            let static_type = if let Expr::Name { name } = &selector.node {
                let key = name.to_lowercase();
                ctx.locals.get(&key).and_then(|info| info.derived_type.clone())
            } else { None };

            if let Some(ref type_name) = static_type {
                if let Some(layout) = ctx.type_layouts.get(type_name) {
                    let tag_val = b.const_i64(layout.type_tag as i64);

                    for guard in guards {
                        match guard {
                            crate::ast::stmt::TypeGuard::TypeIs { type_name: guard_type, body } => {
                                if let Some(guard_layout) = ctx.type_layouts.get(guard_type) {
                                    let guard_tag = b.const_i64(guard_layout.type_tag as i64);
                                    let matches = b.icmp(CmpOp::Eq, tag_val, guard_tag);
                                    let bb_match = b.create_block("type_is_match");
                                    let bb_next = b.create_block("type_is_next");
                                    b.cond_branch(matches, bb_match, vec![], bb_next, vec![]);

                                    b.set_block(bb_match);
                                    lower_stmts(b, ctx, body);
                                    if b.func().block(b.current_block()).terminator.is_none() {
                                        b.branch(bb_end, vec![]);
                                    }

                                    b.set_block(bb_next);
                                } else {
                                    // Unknown guard type — skip.
                                    let tag_matches = type_name.eq_ignore_ascii_case(guard_type);
                                    if tag_matches {
                                        lower_stmts(b, ctx, body);
                                        if b.func().block(b.current_block()).terminator.is_none() {
                                            b.branch(bb_end, vec![]);
                                        }
                                        break;
                                    }
                                }
                            }
                            crate::ast::stmt::TypeGuard::ClassIs { type_name: guard_type, body } => {
                                // CLASS IS matches the type or any extension.
                                // Check if static type is or extends the guard type.
                                let is_match = is_type_or_extends(type_name, guard_type, ctx.type_layouts);
                                if is_match {
                                    lower_stmts(b, ctx, body);
                                    if b.func().block(b.current_block()).terminator.is_none() {
                                        b.branch(bb_end, vec![]);
                                    }
                                    break; // CLASS IS matched, skip remaining guards.
                                }
                            }
                            crate::ast::stmt::TypeGuard::ClassDefault { body } => {
                                lower_stmts(b, ctx, body);
                                if b.func().block(b.current_block()).terminator.is_none() {
                                    b.branch(bb_end, vec![]);
                                }
                            }
                        }
                    }
                }
            }

            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }
            b.set_block(bb_end);
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
            let skip = if ctx.is_alloc_return { Some(ValueId(0)) } else { None };
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts, skip);
            if ctx.is_alloc_return {
                // sret convention: result was written into the hidden first param.
                b.ret(None);
            } else if let Some(addr) = ctx.result_addr {
                let rv = b.load(addr);
                b.ret(Some(rv));
            } else {
                b.ret_void();
            }
        }

        Stmt::Stop { .. } => {
            let skip = if ctx.is_alloc_return { Some(ValueId(0)) } else { None };
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts, skip);
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::ErrorStop { .. } => {
            let skip = if ctx.is_alloc_return { Some(ValueId(0)) } else { None };
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts, skip);
            b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
            b.unreachable();
        }

        Stmt::Allocate { items, opts } => {
            // Resolve STAT= option: find the user's stat variable address.
            // The runtime writes 0 on success or a nonzero error code to this slot.
            // If absent, use a private scratch slot (allocation failure aborts).
            let stat_addr: ValueId = {
                let stat_expr = opts.iter().find(|o| {
                    o.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("stat")).unwrap_or(false)
                });
                if let Some(stat_io) = stat_expr {
                    if let Expr::Name { name } = &stat_io.value.node {
                        if let Some(stat_info) = ctx.locals.get(&name.to_lowercase()) {
                            // Pass the user's variable address directly: runtime writes
                            // 0 (success) or error code into it, so the variable is set.
                            stat_info.addr
                        } else {
                            b.alloca(IrType::Int(IntWidth::I32))
                        }
                    } else {
                        b.alloca(IrType::Int(IntWidth::I32))
                    }
                } else {
                    b.alloca(IrType::Int(IntWidth::I32))
                }
            };

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
                                // Build a stack DimDescriptor[rank] honoring
                                // each subscript's actual (lower, upper) bounds,
                                // then call afs_allocate_array. Both 1-D and
                                // multi-D go through the same path now.
                                let es = b.const_i64(elem_size_bytes);
                                let rank = args.len();
                                let dim_buf_bytes = (rank * 24) as u64;
                                let dim_buf = b.alloca(IrType::Array(
                                    Box::new(IrType::Int(IntWidth::I8)),
                                    dim_buf_bytes,
                                ));
                                let one_i64 = b.const_i64(1);
                                for (i, arg) in args.iter().enumerate() {
                                    let (lo64, up64) =
                                        lower_alloc_bounds(b, &ctx.locals, &arg.value, ctx.st);
                                    let base = (i * 24) as i64;
                                    let off_lo = b.const_i64(base);
                                    let off_up = b.const_i64(base + 8);
                                    let off_st = b.const_i64(base + 16);
                                    let p_lo = b.gep(dim_buf, vec![off_lo], IrType::Int(IntWidth::I8));
                                    let p_up = b.gep(dim_buf, vec![off_up], IrType::Int(IntWidth::I8));
                                    let p_st = b.gep(dim_buf, vec![off_st], IrType::Int(IntWidth::I8));
                                    b.store(lo64, p_lo);
                                    b.store(up64, p_up);
                                    b.store(one_i64, p_st);
                                }
                                let rank_val = b.const_i32(rank as i32);
                                b.call(
                                    FuncRef::External("afs_allocate_array".into()),
                                    vec![info.addr, es, rank_val, dim_buf, stat_addr],
                                    IrType::Void,
                                );
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

        Stmt::Block { decls, body, .. } => {
            // F2008 BLOCK: declarations create new locals scoped to
            // the body, then statements execute.  We alloc+init the
            // decls into the current locals map (no separate scope
            // yet — a future refinement would push/pop a scope).
            if !decls.is_empty() {
                alloc_decls(b, &mut ctx.locals, decls, &HashMap::new(), ctx.type_layouts, &mut Vec::new(), &String::new());
                init_decls(b, &ctx.locals, decls, ctx.st);
            }
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
                ctx.locals.insert(name.to_lowercase(), LocalInfo { addr, ty, dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
            }
            lower_stmts(b, ctx, body);

            // Remove associate names from scope.
            for key in &added_keys {
                ctx.locals.remove(key);
            }
        }

        Stmt::Continue { label: Some(lbl) } => {
            // Labeled CONTINUE: fall through to the label's block.
            if let Some(&label_bb) = ctx.label_blocks.get(lbl) {
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(label_bb, vec![]);
                }
                b.set_block(label_bb);
            }
        }
        Stmt::Continue { label: None } => {} // no-op

        Stmt::Goto { label } => {
            if let Some(&target_bb) = ctx.label_blocks.get(label) {
                b.branch(target_bb, vec![]);
            }
        }

        Stmt::Labeled { label, stmt: inner } => {
            // Create an edge from the current block into the label's block (fall-through),
            // then switch to the label's block and lower the inner statement.
            if let Some(&label_bb) = ctx.label_blocks.get(label) {
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(label_bb, vec![]);
                }
                b.set_block(label_bb);
            }
            lower_stmt(b, ctx, inner);
        }

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
            let unit_i32 = coerce_to_type(b, unit, &IrType::Int(IntWidth::I32));
            let recl_i64 = coerce_to_type(b, recl_val, &IrType::Int(IntWidth::I64));

            // Check if we have any extended specifiers beyond the basic 7-arg set.
            let has_access = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("access")).unwrap_or(false));
            let has_form = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("form")).unwrap_or(false));
            let has_recl = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("recl")).unwrap_or(false));
            let has_position = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("position")).unwrap_or(false));

            if !has_access && !has_form && !has_recl && !has_position {
                // Simple case: use 7-arg afs_open_simple (unit + 3 string pairs).
                b.call(
                    FuncRef::External("afs_open_simple".into()),
                    vec![unit_i32, file_ptr, file_len, status_ptr, status_len, action_ptr, action_len],
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

                let store_at = |b: &mut crate::ir::builder::FuncBuilder,
                                base,
                                offset: i64,
                                field_ty: IrType,
                                val| {
                    let field_bytes = field_ty.size_bytes() as i64;
                    debug_assert!(field_bytes > 0 && offset % field_bytes == 0);
                    let slot = b.const_i64(offset / field_bytes);
                    let ptr = b.gep(base, vec![slot], field_ty.clone());
                    let stored = match field_ty {
                        IrType::Int(_) | IrType::Float(_) | IrType::Bool => {
                            coerce_to_type(b, val, &field_ty)
                        }
                        _ => val,
                    };
                    b.store(stored, ptr);
                };

                let file_ptr_ty = b.func().value_type(file_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let status_ptr_ty = b.func().value_type(status_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let action_ptr_ty = b.func().value_type(action_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let access_ptr_ty = b.func().value_type(access_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let form_ptr_ty = b.func().value_type(form_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let position_ptr_ty = b.func().value_type(position_ptr).unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));

                store_at(b, cb, 0, IrType::Int(IntWidth::I32), unit_i32);
                store_at(b, cb, 8, file_ptr_ty, file_ptr);
                store_at(b, cb, 16, IrType::Int(IntWidth::I64), file_len);
                store_at(b, cb, 24, status_ptr_ty, status_ptr);
                store_at(b, cb, 32, IrType::Int(IntWidth::I64), status_len);
                store_at(b, cb, 40, action_ptr_ty, action_ptr);
                store_at(b, cb, 48, IrType::Int(IntWidth::I64), action_len);
                store_at(b, cb, 56, access_ptr_ty, access_ptr);
                store_at(b, cb, 64, IrType::Int(IntWidth::I64), access_len);
                store_at(b, cb, 72, form_ptr_ty, form_ptr);
                store_at(b, cb, 80, IrType::Int(IntWidth::I64), form_len);
                store_at(b, cb, 88, IrType::Int(IntWidth::I64), recl_i64);
                store_at(b, cb, 96, IrType::Int(IntWidth::I64), null);       // iostat = null
                store_at(b, cb, 104, IrType::Int(IntWidth::I64), null);      // newunit = null
                store_at(b, cb, 112, position_ptr_ty, position_ptr);
                store_at(b, cb, 120, IrType::Int(IntWidth::I64), position_len);

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
            let fmt_control = controls.iter().skip(1)
                .find(|c| c.keyword.is_none())
                .or_else(|| controls.iter().find(|c| c.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("fmt")).unwrap_or(false)));

            let is_list_directed = match fmt_control {
                None => true,
                Some(ctrl) => matches!(&ctrl.value.node, Expr::Name { name } if name == "*"),
            };

            if let Some(ctrl) = controls.first() {
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_read_items(b, ctx, items, buf_ptr, buf_len);
                    } else {
                        let (fmt_ptr, fmt_len) =
                            lower_string_expr(b, &ctx.locals, &fmt_control.unwrap().value, ctx.st);
                        lower_formatted_internal_read_items(
                            b, ctx, items, buf_ptr, buf_len, fmt_ptr, fmt_len,
                        );
                    }
                    return;
                }
            }

            // READ(unit, *) items — simplified: first control is unit.
            let unit = if let Some(ctrl) = controls.first() {
                lower_expr(b, &ctx.locals, &ctrl.value, ctx.st)
            } else {
                b.const_i32(5) // default stdin
            };
            if is_list_directed {
                lower_list_read_items(b, ctx, items, unit);
            } else {
                let (fmt_ptr, fmt_len) =
                    lower_string_expr(b, &ctx.locals, &fmt_control.unwrap().value, ctx.st);
                lower_formatted_read_items(b, ctx, items, unit, fmt_ptr, fmt_len);
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

        Stmt::Nullify { items } => {
            // Zero each pointer slot so ASSOCIATED returns false.
            for item in items {
                let Expr::Name { name } = &item.node else { continue; };
                let Some(info) = ctx.locals.get(&name.to_lowercase()) else { continue; };
                if !info.is_pointer { continue; }
                // Array pointers use the 384-byte descriptor (allocatable=true);
                // scalar and DT pointers use an 8-byte slot.
                let size = if info.allocatable { 384i64 } else { 8i64 };
                let zero_byte = b.const_i32(0);
                let sz = b.const_i64(size);
                b.call(
                    FuncRef::External("memset".into()),
                    vec![info.addr, zero_byte, sz],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                );
            }
        }

        Stmt::PointerAssignment { target, value } => {
            // `p => q` or `p => x`: rebind the pointer slot `p` to the
            // address of the RHS designator.  Three shapes:
            //
            //   * scalar + derived-type pointer: slot holds an 8-byte
            //     pointer, `=>` stores the target's address into it.
            //   * array pointer: slot holds a 384-byte ArrayDescriptor,
            //     `=>` materialises a descriptor of the target and
            //     memcpy's it into the slot.
            //
            // In both cases the target must be a simple Name for now;
            // component-access and slice targets are follow-up work.
            let Expr::Name { name: tgt_name } = &target.node else { return; };
            let tgt_key = tgt_name.to_lowercase();
            let Some(tgt_info) = ctx.locals.get(&tgt_key).cloned() else { return; };
            if !tgt_info.is_pointer { return; }

            // Handle section-RHS: pa => ia(lo:hi).  The RHS is a
            // FunctionCall{Name(arr), [Range(lo,hi)]}.  Build a
            // descriptor pointing at arr(lo) with extent hi-lo+1.
            if let Expr::FunctionCall { callee, args: val_args } = &value.node {
                if let Expr::Name { name: arr_name } = &callee.node {
                    let arr_key = arr_name.to_lowercase();
                    if let Some(arr_info) = ctx.locals.get(&arr_key).cloned() {
                        if (!arr_info.dims.is_empty() || arr_info.allocatable) && val_args.len() == 1 {
                            if let crate::ast::expr::SectionSubscript::Range { start, end, stride: _ } = &val_args[0].value {
                                let base = array_data_ptr_for_call(b, &arr_info);
                                let lo = if let Some(se) = start {
                                    let v = lower_expr(b, &ctx.locals, se, ctx.st);
                                    match b.func().value_type(v) {
                                        Some(IrType::Int(IntWidth::I64)) => v,
                                        _ => b.int_extend(v, IntWidth::I64, true),
                                    }
                                } else { b.const_i64(1) };
                                let hi = if let Some(ee) = end {
                                    let v = lower_expr(b, &ctx.locals, ee, ctx.st);
                                    match b.func().value_type(v) {
                                        Some(IrType::Int(IntWidth::I64)) => v,
                                        _ => b.int_extend(v, IntWidth::I64, true),
                                    }
                                } else {
                                    let total = array_total_elems_value(b, &arr_info);
                                    total
                                };
                                // Build a descriptor in the pointer's slot.
                                let desc = tgt_info.addr;
                                let zero32 = b.const_i32(0);
                                let sz384 = b.const_i64(384);
                                b.call(FuncRef::External("memset".into()),
                                    vec![desc, zero32, sz384],
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                // base_addr: base + (lo - 1) * elem_size
                                let one = b.const_i64(1);
                                let lo_0 = b.isub(lo, one);
                                let elem_bytes = b.const_i64(ir_scalar_byte_size(&arr_info.ty));
                                let byte_off = b.imul(lo_0, elem_bytes);
                                let slice_base = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
                                store_byte_aggregate_field(b, desc, 0, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))), slice_base);
                                store_byte_aggregate_field(b, desc, 8, IrType::Int(IntWidth::I64), elem_bytes);
                                let rank = b.const_i32(1);
                                store_byte_aggregate_field(b, desc, 16, IrType::Int(IntWidth::I32), rank);
                                let flags = b.const_i32(2);
                                store_byte_aggregate_field(b, desc, 20, IrType::Int(IntWidth::I32), flags);
                                // dim[0]: lower=1, upper=extent, stride=1
                                store_byte_aggregate_field(b, desc, 24, IrType::Int(IntWidth::I64), one);
                                let extent = b.isub(hi, lo);
                                let extent1 = b.iadd(extent, one);
                                store_byte_aggregate_field(b, desc, 32, IrType::Int(IntWidth::I64), extent1);
                                store_byte_aggregate_field(b, desc, 40, IrType::Int(IntWidth::I64), one);
                                return;
                            }
                        }
                    }
                }
            }

            let Expr::Name { name: src_name } = &value.node else { return; };
            let src_key = src_name.to_lowercase();
            let Some(src_info) = ctx.locals.get(&src_key).cloned() else { return; };

            // Array pointer path: materialise a descriptor from the
            // target and memcpy 384 bytes into the pointer's slot.
            // Both explicit-shape stack arrays and descriptor-backed
            // allocatables are supported via array_data_ptr_for_call.
            let target_is_array = !src_info.dims.is_empty() || src_info.allocatable || src_info.descriptor_arg;
            if target_is_array {
                let src_desc = if local_uses_array_descriptor(&src_info) {
                    array_descriptor_addr(b, &src_info)
                } else {
                    materialize_array_descriptor_for_info(b, &src_info)
                };
                let size = b.const_i64(384);
                b.call(
                    FuncRef::External("memcpy".into()),
                    vec![tgt_info.addr, src_desc, size],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                );
                return;
            }

            // Scalar / derived-type pointer path.
            let addr = if src_info.is_pointer {
                // Copy the current association of another pointer
                // (pointer-to-pointer, including derived-type pointer
                // chains).  For scalar pointers (ty = i32) the stored
                // value is Ptr<i32>; for DT pointers (ty = Ptr<i8>)
                // the stored value is already Ptr<i8> — wrapping
                // again would produce Ptr<Ptr<i8>> and fail the
                // verifier.  Use ty directly when it's already a
                // pointer.
                let load_ty = if src_info.ty.is_ptr() {
                    src_info.ty.clone()
                } else {
                    IrType::Ptr(Box::new(src_info.ty.clone()))
                };
                b.load_typed(src_info.addr, load_ty)
            } else if src_info.derived_type.is_some() {
                // Derived-type TARGET.  src_info.addr is a
                // ptr<[i8 x size]>; the pointer slot expects ptr<i8>.
                // A zero-offset GEP with element type i8 produces
                // the element-pointer view and round-trips through
                // the verifier.
                let zero = b.const_i64(0);
                b.gep(src_info.addr, vec![zero], IrType::Int(IntWidth::I8))
            } else {
                // Plain TARGET or ordinary scalar local: the alloca
                // address IS the associated target.
                src_info.addr
            };
            b.store(addr, tgt_info.addr);
        }

        _ => {} // remaining statements (FORALL, WHERE, etc.) deferred
    }
}

/// Returns true if an expression contains no function calls, I/O, or
/// other side effects. Used by Select lowering to ensure both branches
/// are safe to evaluate unconditionally.
fn is_pure_expr(expr: &crate::ast::expr::Expr) -> bool {
    use crate::ast::expr::Expr;
    match expr {
        // Leaf nodes — always pure.
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::ComplexLiteral { .. }
        | Expr::BozLiteral { .. }
        | Expr::Name { .. } => true,

        // Binary/unary — pure if operands are pure.
        Expr::BinaryOp { left, right, .. } => {
            is_pure_expr(&left.node) && is_pure_expr(&right.node)
        }
        Expr::UnaryOp { operand, .. } => is_pure_expr(&operand.node),
        Expr::ParenExpr { inner } => is_pure_expr(&inner.node),

        // Function calls, array constructors, component access — not pure
        // (function calls have side effects; component access and array
        // constructors may involve complex lowering).
        _ => false,
    }
}

/// Try to lower `if (cond) x = a; else x = b` as a Select instruction
/// instead of a diamond of basic blocks. Returns true on success.
///
/// Detection criteria (all must hold):
///   1. No else-ifs.
///   2. Then body has exactly 1 statement — a scalar assignment to a Name.
///   3. Else body has exactly 1 statement — a scalar assignment to the
///      **same** Name.
///   4. The target variable is a non-character, non-array, non-allocatable
///      local (a simple alloca scalar).
///
/// When this fires, the result is a single `Select` + `Store`, enabling
/// ARM64 `CSEL` instruction selection.
fn try_lower_select(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    condition: &crate::ast::expr::SpannedExpr,
    then_body: &[SpannedStmt],
    else_ifs: &[(crate::ast::expr::SpannedExpr, Vec<SpannedStmt>)],
    else_body: &Option<Vec<SpannedStmt>>,
) -> bool {
    use crate::ast::expr::Expr;
    use crate::ast::stmt::Stmt;

    // Only simple if/else — no else-if chain.
    if !else_ifs.is_empty() { return false; }
    let eb = match else_body { Some(eb) => eb, None => return false };

    // Exactly one statement in each branch.
    if then_body.len() != 1 || eb.len() != 1 { return false; }

    // Both must be simple scalar assignments (Stmt::Assignment to a Name).
    let (then_name, then_val_expr) = match &then_body[0].node {
        Stmt::Assignment { target, value } => match &target.node {
            Expr::Name { name } => (name.to_lowercase(), value),
            _ => return false,
        },
        _ => return false,
    };
    let (else_name, else_val_expr) = match &eb[0].node {
        Stmt::Assignment { target, value } => match &target.node {
            Expr::Name { name } => (name.to_lowercase(), value),
            _ => return false,
        },
        _ => return false,
    };

    // Both must assign to the same variable.
    if then_name != else_name { return false; }

    // The variable must be a simple scalar local (not character, not array,
    // not allocatable). These constraints ensure a plain store suffices.
    let info = match ctx.locals.get(&then_name) {
        Some(info) => info.clone(),
        None => return false,
    };
    if !info.dims.is_empty() || info.allocatable { return false; }
    if !matches!(info.char_kind, CharKind::None) { return false; }

    // Both RHS expressions must be side-effect-free (no function calls,
    // no I/O). Select evaluates BOTH branches unconditionally, so a
    // call like `fib(n-1)` in an else branch would execute even when
    // the condition is true — causing infinite recursion.
    if !is_pure_expr(&then_val_expr.node) { return false; }
    if !is_pure_expr(&else_val_expr.node) { return false; }

    // Lower condition, then both value expressions, then emit Select + Store.
    let cond = lower_expr(b, &ctx.locals, condition, ctx.st);
    let tv = lower_expr(b, &ctx.locals, then_val_expr, ctx.st);
    let fv = lower_expr(b, &ctx.locals, else_val_expr, ctx.st);
    let selected = b.select(cond, tv, fv);
    b.store(selected, info.addr);
    true
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
    // Fast path: simple diamond `if (cond) x = a; else x = b` → Select.
    if try_lower_select(b, ctx, condition, then_body, else_ifs, else_body) {
        return;
    }

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
    concurrent: bool,
}

fn try_lower_bulk_do_concurrent(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    controls: &[ConcurrentControl],
    mask: Option<&crate::ast::expr::SpannedExpr>,
    body: &[SpannedStmt],
) -> bool {
    if mask.is_some() || controls.len() != 1 || body.len() != 1 {
        return false;
    }
    let ctrl = &controls[0];
    let Stmt::Assignment { target, value } = &body[0].node else {
        return false;
    };
    let Some(dest) = loop_indexed_array_ref(&ctx.locals, target, &ctrl.var) else {
        return false;
    };
    if !control_covers_full_array(ctrl, &dest) {
        return false;
    }
    let Some(plan) = build_loop_bulk_plan(&ctx.locals, &dest.info, &ctrl.var, value) else {
        return false;
    };
    let n = array_total_elems_value(b, &dest.info);
    emit_bulk_array_plan(b, ctx, &dest.info, n, plan);
    true
}

fn lower_do_concurrent(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    name: &Option<String>,
    controls: &[ConcurrentControl],
    mask: Option<&crate::ast::expr::SpannedExpr>,
    body: &[SpannedStmt],
    span: crate::lexer::Span,
) {
    if try_lower_bulk_do_concurrent(b, ctx, controls, mask, body) {
        return;
    }

    let Some((ctrl, rest)) = controls.split_first() else {
        return;
    };

    let var_opt = Some(ctrl.var.clone());
    let start_opt = Some(ctrl.start.clone());
    let end_opt = Some(ctrl.end.clone());

    let nested_body_storage;
    let masked_body_storage;
    let lowered_body = if rest.is_empty() {
        if let Some(mask_expr) = mask {
            masked_body_storage = vec![crate::ast::Spanned::new(
                Stmt::IfConstruct {
                    name: None,
                    condition: mask_expr.clone(),
                    then_body: body.to_vec(),
                    else_ifs: vec![],
                    else_body: None,
                },
                mask_expr.span,
            )];
            masked_body_storage.as_slice()
        } else {
            body
        }
    } else {
        nested_body_storage = vec![crate::ast::Spanned::new(
            Stmt::DoConcurrent {
                name: None,
                controls: rest.to_vec(),
                mask: mask.cloned(),
                locality: vec![],
                body: body.to_vec(),
            },
            span,
        )];
        nested_body_storage.as_slice()
    };

    lower_do_loop(b, ctx, DoLoopFields {
        name,
        var: &var_opt,
        start: &start_opt,
        end: &end_opt,
        step: &ctrl.step,
        body: lowered_body,
        concurrent: true,
    });
}

/// Lower DO loop (counted loop with variable, start, end, step).
fn lower_do_loop(b: &mut FuncBuilder, ctx: &mut LowerCtx, fields: DoLoopFields) {
    let DoLoopFields { name, var, start, end, step, body, concurrent } = fields;
    let (check_name, body_name, incr_name, exit_name, neg_check_name, pos_check_name) = if concurrent {
        ("doconc_check", "doconc_body", "doconc_incr", "doconc_exit", "doconc_neg_check", "doconc_pos_check")
    } else {
        ("do_check", "do_body", "do_incr", "do_exit", "do_neg_check", "do_pos_check")
    };
    if let (Some(var_name), Some(start_expr), Some(end_expr)) = (var, start, end) {
        // Counted DO loop.
        let key = var_name.to_lowercase();
        let var_addr = ctx.locals.get(&key).map(|info| info.addr).unwrap_or_else(|| {
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            ctx.locals.insert(key.clone(), LocalInfo { addr, ty: IrType::Int(IntWidth::I32), dims: vec![], allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None, derived_type: None, inline_const: None, is_pointer: false });
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

        let bb_check = b.create_block(check_name);
        let bb_body = b.create_block(body_name);
        let bb_incr = b.create_block(incr_name);
        let bb_exit = b.create_block(exit_name);

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
            let bb_neg_check = b.create_block(neg_check_name);
            let bb_pos_check = b.create_block(pos_check_name);
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
        let bb_body = b.create_block(body_name);
        let bb_exit = b.create_block(exit_name);
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
    let idx64 = compute_flat_elem_offset(b, locals, info, args, st);
    let base = array_base_addr(b, info);
    let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
    b.load(elem_ptr)
}

fn emit_bounds_check(
    b: &mut FuncBuilder,
    index: ValueId,
    lower: ValueId,
    upper: ValueId,
) {
    b.runtime_call(RuntimeFunc::CheckBounds, vec![index, lower, upper], IrType::Void);
}

/// Compute the column-major flat ELEMENT offset (i64) for an array
/// subscript expression, returning a value suitable for `b.gep` (which
/// scales by the GEP result element size).
///
/// Two paths:
///   * Static-shape arrays (`info.dims` populated): fold strides at
///     compile time.
///   * Allocatable arrays (rank/extents only known at runtime): load
///     lower_bound and upper_bound from the runtime descriptor and
///     accumulate the cumulative stride as a runtime i64.
///
/// Audit5 MAJOR-1: previously both lower_array_element and
/// lower_array_store fell back to `(1, 1)` for every dim of an
/// allocatable, leaving cumulative stride = 1. m(i, j) for a 3x4
/// allocatable then computed `(i-1) + (j-1)` instead of
/// `(i-1) + (j-1)*3`, so writes clobbered each other and reads
/// returned garbage.
fn compute_flat_elem_offset(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    st: &SymbolTable,
) -> ValueId {
    if local_uses_array_descriptor(info) {
        // Runtime descriptor path. Each DimDescriptor is 24 bytes
        // starting at descriptor offset 24:
        //   +0  lower_bound : i64
        //   +8  upper_bound : i64
        //   +16 stride      : i64 (we use 1)
        let desc = array_descriptor_addr(b, info);
        let mut flat: Option<ValueId> = None;
        let mut cum_stride: Option<ValueId> = None; // i64
        let one64 = b.const_i64(1);
        for (dim_idx, arg) in args.iter().enumerate() {
            let sub_raw = match &arg.value {
                crate::ast::expr::SectionSubscript::Element(e) => {
                    lower_expr(b, locals, e, st)
                }
                _ => b.const_i64(0),
            };
            let sub = widen_idx_to_i64(b, sub_raw);

            let dim_base = 24 + (dim_idx as i64) * 24;
            let off_lo = b.const_i64(dim_base);
            let off_up = b.const_i64(dim_base + 8);
            let p_lo = b.gep(desc, vec![off_lo], IrType::Int(IntWidth::I8));
            let p_up = b.gep(desc, vec![off_up], IrType::Int(IntWidth::I8));
            let lo = b.load_typed(p_lo, IrType::Int(IntWidth::I64));
            let up = b.load_typed(p_up, IrType::Int(IntWidth::I64));
            emit_bounds_check(b, sub, lo, up);

            let adjusted = b.isub(sub, lo);

            let dim_offset = match cum_stride {
                None => adjusted, // first dim has cumulative stride 1
                Some(s) => b.imul(adjusted, s),
            };
            flat = Some(match flat {
                Some(prev) => b.iadd(prev, dim_offset),
                None => dim_offset,
            });

            // cum_stride *= (upper - lower + 1)
            let span = b.isub(up, lo);
            let extent = b.iadd(span, one64);
            cum_stride = Some(match cum_stride {
                None => extent,
                Some(prev) => b.imul(prev, extent),
            });
        }
        return flat.unwrap_or_else(|| b.const_i64(0));
    }

    // Static-shape path: fold strides at compile time.
    let mut flat_offset: Option<ValueId> = None;
    let mut stride: i64 = 1;

    for (dim_idx, arg) in args.iter().enumerate() {
        let subscript = match &arg.value {
            crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
            _ => b.const_i32(0),
        };
        let subscript64 = widen_idx_to_i64(b, subscript);

        let (lower, extent) = if dim_idx < info.dims.len() {
            info.dims[dim_idx]
        } else {
            (1, 1)
        };
        let upper = lower + extent - 1;
        let lower_val = b.const_i64(lower);
        let upper_val = b.const_i64(upper);
        emit_bounds_check(b, subscript64, lower_val, upper_val);
        let adjusted = b.isub(subscript64, lower_val);

        let dim_offset = if stride == 1 {
            adjusted
        } else {
            let stride_val = b.const_i64(stride);
            b.imul(adjusted, stride_val)
        };

        flat_offset = Some(match flat_offset {
            Some(prev) => b.iadd(prev, dim_offset),
            None => dim_offset,
        });

        stride *= extent;
    }

    flat_offset.unwrap_or_else(|| b.const_i64(0))
}

/// Widen an i32 (or smaller) index value to i64 for pointer
/// arithmetic. Pass through for values already i64 or larger.
fn widen_idx_to_i64(b: &mut FuncBuilder, idx: ValueId) -> ValueId {
    match b.func().value_type(idx) {
        Some(IrType::Int(IntWidth::I64)) => idx,
        Some(IrType::Int(_)) => b.int_extend(idx, IntWidth::I64, true),
        _ => idx,
    }
}

/// Lower an ALLOCATE bound subscript to (lower_bound, upper_bound)
/// as i64 values. Both forms are valid:
///
///   allocate(a(N))      → Element(N)        → (1, N)
///   allocate(a(0:N))    → Range(0, N)       → (0, N)
///   allocate(a(lo:hi))  → Range(lo, hi)     → (lo, hi)
///
/// A bare `Range { start: None, .. }` defaults the lower bound
/// to 1 (Fortran convention). A `Range` with no `end` is
/// invalid in ALLOCATE — defaults to 1 to keep the runtime
/// from segfaulting and let the verifier catch it.
///
/// Audit6 BLOCKING-4: the previous Stmt::Allocate code only
/// handled Element subscripts and silently dropped the Range
/// case to const_i64(1), causing heap corruption on
/// `allocate(m(0:2, 0:3))`.
fn lower_alloc_bounds(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    sub: &crate::ast::expr::SectionSubscript,
    st: &SymbolTable,
) -> (ValueId, ValueId) {
    use crate::ast::expr::SectionSubscript;
    match sub {
        SectionSubscript::Element(e) => {
            let up = lower_expr(b, locals, e, st);
            let up64 = widen_idx_to_i64(b, up);
            let lo64 = b.const_i64(1);
            (lo64, up64)
        }
        SectionSubscript::Range { start, end, .. } => {
            let lo64 = match start {
                Some(e) => {
                    let v = lower_expr(b, locals, e, st);
                    widen_idx_to_i64(b, v)
                }
                None => b.const_i64(1),
            };
            let up64 = match end {
                Some(e) => {
                    let v = lower_expr(b, locals, e, st);
                    widen_idx_to_i64(b, v)
                }
                None => b.const_i64(1),
            };
            (lo64, up64)
        }
    }
}

/// Element-byte size for an IR scalar type. Used by array
/// constructor lowering to compute byte offsets into a destination
/// buffer. Defaults to 8 for unknown/wide types so we never
/// under-step (a wrong-direction error would silently scribble
/// over adjacent elements).
fn ir_scalar_byte_size(ty: &IrType) -> i64 {
    match ty {
        IrType::Int(IntWidth::I8) | IrType::Bool => 1,
        IrType::Int(IntWidth::I16) => 2,
        IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
        IrType::Int(IntWidth::I128) => 16,
        _ => 8,
    }
}

/// Store the literal values of an array constructor into a
/// destination buffer, one element at a time via byte-level GEP.
///
/// `dest_base` is a byte pointer to the start of the buffer
/// (already loaded from a descriptor if the dest is allocatable).
/// `elem_ty` is the element type used to coerce/size each value.
///
/// Handles both literal expressions and implied-do iterators.
/// Literal expressions use a compile-time byte offset; implied-do
/// iterators generate a real runtime loop that advances an
/// alloca-backed offset. The DO variable is installed in a
/// clone of `locals` so the inner expression can reference it.
///
/// Audit BLOCKING-1: previously the implied-do branch silently
/// skipped all stores and advanced a compile-time counter,
/// leaving the destination buffer with whatever stack bytes
/// happened to be there (the comment lied about allocas being
/// zeroed). Programs that used `[(expr, i=1,n)]` got garbage.
fn store_ac_values_into(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    dest_base: ValueId,
    elem_ty: &IrType,
    values: &[crate::ast::expr::AcValue],
    st: &SymbolTable,
) {
    let elem_bytes = ir_scalar_byte_size(elem_ty);
    // Runtime byte offset. Starts at 0 and is bumped by elem_bytes
    // after each store. Using an alloca (not a ValueId) lets the
    // implied-do loop body update the offset across iterations.
    let off_slot = b.alloca(IrType::Int(IntWidth::I64));
    let zero64 = b.const_i64(0);
    b.store(zero64, off_slot);
    let step_bytes = b.const_i64(elem_bytes);

    for v in values {
        match v {
            crate::ast::expr::AcValue::Expr(e) => {
                let raw = lower_expr(b, locals, e, st);
                let coerced = coerce_to_type(b, raw, elem_ty);
                let cur_off = b.load(off_slot);
                let p = b.gep(dest_base, vec![cur_off], IrType::Int(IntWidth::I8));
                b.store(coerced, p);
                let next_off = b.iadd(cur_off, step_bytes);
                b.store(next_off, off_slot);
            }
            crate::ast::expr::AcValue::ImpliedDo(ido) => {
                store_ac_implied_do(
                    b, locals, dest_base, elem_ty, elem_bytes, off_slot,
                    &ido.values, &ido.var, &ido.start, &ido.end, ido.step.as_ref(), st,
                );
            }
        }
    }
}

/// Lower an implied-do array constructor iterator:
///   `( inner_values, var = start, end [, step] )`
/// produces the sequence `inner_values[var=start], inner_values[var=start+step], …`.
/// Each iteration evaluates the inner value list with `var` bound
/// to the current iteration, stores them at the current offset,
/// and advances the offset. The DO variable is installed into a
/// scratch clone of `locals` for the duration of the iterator.
#[allow(clippy::too_many_arguments)]
fn store_ac_implied_do(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    dest_base: ValueId,
    elem_ty: &IrType,
    elem_bytes: i64,
    off_slot: ValueId,
    inner: &[crate::ast::expr::AcValue],
    var: &str,
    start: &crate::ast::expr::SpannedExpr,
    end: &crate::ast::expr::SpannedExpr,
    step: Option<&crate::ast::expr::SpannedExpr>,
    st: &SymbolTable,
) {
    // DO variable as a fresh i32 alloca, installed in a scratch
    // locals map so the inner expressions can reference it.
    let var_ty = IrType::Int(IntWidth::I32);
    let var_addr = b.alloca(var_ty.clone());
    let start_val = lower_expr(b, locals, start, st);
    let start_coerced = coerce_to_type(b, start_val, &var_ty);
    b.store(start_coerced, var_addr);

    let end_val = lower_expr(b, locals, end, st);
    let end_coerced = coerce_to_type(b, end_val, &var_ty);

    let step_val_raw = match step {
        Some(e) => lower_expr(b, locals, e, st),
        None => b.const_i32(1),
    };
    let step_val = coerce_to_type(b, step_val_raw, &var_ty);

    let mut scratch_locals = locals.clone();
    scratch_locals.insert(var.to_lowercase(), LocalInfo {
        addr: var_addr,
        ty: var_ty.clone(),
        dims: vec![],
        allocatable: false,
        descriptor_arg: false,
        by_ref: false,
        char_kind: CharKind::None,
        derived_type: None, inline_const: None, is_pointer: false,
    });

    // Loop skeleton: check → body → exit. Mirrors the regular DO
    // lowerer's sign-of-step handling: if `step` is a compile-time
    // constant, pick `Le` for positive and `Ge` for negative; if
    // it's a runtime value, branch on the sign and emit two check
    // arms. Audit BLOCKING-1: the previous version hardcoded `Le`
    // and `[(i, i=5,1,-1)]` skipped the body entirely.
    let check = b.create_block("ac_impdo_check");
    let body  = b.create_block("ac_impdo_body");
    let exit  = b.create_block("ac_impdo_exit");
    b.branch(check, vec![]);

    b.set_block(check);
    let cur_var = b.load(var_addr);
    let const_step = step.and_then(eval_const_int);
    if let Some(sv) = const_step {
        let cmp_op = if sv < 0 { CmpOp::Ge } else { CmpOp::Le };
        let cond = b.icmp(cmp_op, cur_var, end_coerced);
        b.cond_branch(cond, body, vec![], exit, vec![]);
    } else {
        // Runtime step: branch on sign at the check site so we
        // pick the correct comparison without recomputing on each
        // iteration. Two check sub-blocks, one per direction.
        let zero = b.const_i32(0);
        let step_neg = b.icmp(CmpOp::Lt, step_val, zero);
        let bb_neg = b.create_block("ac_impdo_neg_check");
        let bb_pos = b.create_block("ac_impdo_pos_check");
        b.cond_branch(step_neg, bb_neg, vec![], bb_pos, vec![]);

        b.set_block(bb_neg);
        let cond_neg = b.icmp(CmpOp::Ge, cur_var, end_coerced);
        b.cond_branch(cond_neg, body, vec![], exit, vec![]);

        b.set_block(bb_pos);
        let cond_pos = b.icmp(CmpOp::Le, cur_var, end_coerced);
        b.cond_branch(cond_pos, body, vec![], exit, vec![]);
    }

    // Body: evaluate each inner value and store at the current
    // offset. Recurses into store_ac_values_into so nested
    // implied-do works.
    b.set_block(body);
    for iv in inner {
        match iv {
            crate::ast::expr::AcValue::Expr(e) => {
                let raw = lower_expr(b, &scratch_locals, e, st);
                let coerced = coerce_to_type(b, raw, elem_ty);
                let cur_off = b.load(off_slot);
                let p = b.gep(dest_base, vec![cur_off], IrType::Int(IntWidth::I8));
                b.store(coerced, p);
                let step_bytes = b.const_i64(elem_bytes);
                let next_off = b.iadd(cur_off, step_bytes);
                b.store(next_off, off_slot);
            }
            crate::ast::expr::AcValue::ImpliedDo(ido) => {
                store_ac_implied_do(
                    b, &scratch_locals, dest_base, elem_ty, elem_bytes, off_slot,
                    &ido.values, &ido.var, &ido.start, &ido.end, ido.step.as_ref(), st,
                );
            }
        }
    }
    // Advance the DO variable and loop.
    let cur_var_end = b.load(var_addr);
    let next_var = b.iadd(cur_var_end, step_val);
    b.store(next_var, var_addr);
    b.branch(check, vec![]);

    // Continue emitting into exit.
    b.set_block(exit);
}

fn lower_char_array_store(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    value: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) {
    let idx64 = compute_flat_elem_offset(b, locals, info, args, st);
    let base = array_base_addr(b, info);
    let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
    let dest_ptr = b.load_typed(elem_ptr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
    let (src_ptr, src_len) = lower_string_expr(b, locals, value, st);
    let len = match info.char_kind {
        CharKind::Fixed(len) => len,
        _ => 0,
    };
    let dest_len = b.const_i64(len);
    b.call(
        FuncRef::External("afs_assign_char_fixed".into()),
        vec![dest_ptr, dest_len, src_ptr, src_len],
        IrType::Void,
    );
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
    let idx64 = compute_flat_elem_offset(b, locals, info, args, st);
    let base = array_base_addr(b, info);
    let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
    // Audit5 CRITICAL-1: coerce the RHS to the array element
    // type before the store. Without this, an i32-typed value
    // assigned into an i8 array would emit a 4-byte STR through
    // a 1-byte slot, clobbering the next 3 bytes. The verifier's
    // store-pointee width check has a `pointee_is_byte` escape
    // hatch for derived-type byte-cursor GEPs, so the bad store
    // wasn't caught at the IR level either.
    let coerced = coerce_to_type(b, value, &info.ty);
    b.store(coerced, elem_ptr);
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
        let is_char = match &item.node {
            Expr::Name { name } => ctx.locals.get(&name.to_lowercase())
                .map(|i| i.char_kind != CharKind::None)
                .unwrap_or(false),
            Expr::FunctionCall { callee, args } => {
                if let Expr::Name { name } = &callee.node {
                    let key = name.to_lowercase();
                    matches!(key.as_str(),
                        "trim" | "adjustl" | "adjustr" | "char")
                        || ctx.locals.get(&key)
                            .map(|i| i.char_kind != CharKind::None && i.dims.is_empty())
                            .unwrap_or(false)
                } else if let Expr::FunctionCall { callee: inner, .. } = &callee.node {
                    // Nested: arr(i)(lo:hi) — substring of char array element.
                    if let Expr::Name { name } = &inner.node {
                        let key = name.to_lowercase();
                        ctx.locals.get(&key)
                            .map(|i| i.char_kind != CharKind::None && (!i.dims.is_empty() || i.allocatable))
                            .unwrap_or(false)
                            && args.iter().any(|a| matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. }))
                    } else { false }
                } else { false }
            }
            _ => false,
        };

        // Whole-array print: a plain Name reference whose local
        // has array dims. Iterate elements and call the per-element
        // write helper. Without this, the Ptr<_> the item lowers to
        // would fall into the IrType::Ptr arm below and dispatch to
        // afs_write_string with a bogus length.
        //
        // Also handles 1-D array slices `a(lo:hi)` and `a(lo:hi:step)`
        // by detecting a FunctionCall with a Range subscript on an
        // array name. Slices bypass the section-descriptor code
        // path (which crashes in afs_create_section for a bare
        // write item) and instead lower directly into a bounded
        // loop over the underlying base.
        if !is_char {
            if let Expr::Name { name } = &item.node {
                let key = name.to_lowercase();
                if let Some(info) = ctx.locals.get(&key).cloned() {
                    if is_complex_ty(&info.ty) {
                        // Complex variable: pass pointer to [f32/f64 x 2] buffer.
                        // For a POINTER complex, the slot holds the
                        // target buffer address — load it first.
                        let addr = if info.is_pointer {
                            b.load_typed(info.addr, IrType::Ptr(Box::new(info.ty.clone())))
                        } else if info.by_ref {
                            b.load(info.addr)
                        } else {
                            info.addr
                        };
                        let func = if matches!(info.ty, IrType::Array(ref e, 2)
                                if matches!(e.as_ref(), IrType::Float(FloatWidth::F64))) {
                            "afs_write_complex_f64"
                        } else {
                            "afs_write_complex_f32"
                        };
                        b.call(FuncRef::External(func.into()), vec![unit, addr], IrType::Void);
                        continue;
                    }
                    if !info.dims.is_empty() || info.allocatable {
                        lower_whole_array_write(b, ctx, &info, unit);
                        continue;
                    }
                }
            }
            // Complex literal in print position: detect ptr<[f32/f64 x 2]>
            if matches!(item.node, Expr::ComplexLiteral { .. }) {
                let addr = lower_expr_tl(b, &ctx.locals, item, ctx.st, ctx.type_layouts);
                // Default to f32 — if literal had f64 components lower_expr would
                // have allocated [f64 x 2]. Check the original node to be precise.
                let func = if let Expr::ComplexLiteral { real, imag } = &item.node {
                    let is_double = |e: &crate::ast::expr::SpannedExpr| {
                        if let Expr::RealLiteral { text, .. } = &e.node {
                            text.to_lowercase().contains('d')
                        } else { false }
                    };
                    if is_double(real) || is_double(imag) { "afs_write_complex_f64" }
                    else { "afs_write_complex_f32" }
                } else { "afs_write_complex_f32" };
                b.call(FuncRef::External(func.into()), vec![unit, addr], IrType::Void);
                continue;
            }
            if let Expr::FunctionCall { callee, args } = &item.node {
                if let Expr::Name { name } = &callee.node {
                    let key = name.to_lowercase();
                    if let Some(info) = ctx.locals.get(&key).cloned() {
                        if !info.dims.is_empty() || info.allocatable {
                            let has_range = args.iter().any(|a|
                                matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. }));
                            if has_range {
                                if args.len() == 1 {
                                    lower_1d_slice_write(b, ctx, &info, &args[0], unit);
                                } else {
                                    // Audit CRITICAL-3: multi-dim
                                    // slice prints used to fall
                                    // through to afs_create_section
                                    // on a bare stack pointer and
                                    // crash. Now lowered as nested
                                    // column-major loops directly.
                                    lower_section_write_nd(b, ctx, &info, args, unit);
                                }
                                continue;
                            }
                        }
                    }
                }
            }
        }

        if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
            let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
            b.call(FuncRef::External("afs_write_string".into()), vec![unit, ptr, len], IrType::Void);
        } else {
            let val = lower_expr_tl(b, &ctx.locals, item, ctx.st, ctx.type_layouts);
            let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
            let func_name = match &ty {
                IrType::Int(IntWidth::I128) => "afs_write_int128",
                IrType::Int(IntWidth::I64) => "afs_write_int64",
                IrType::Int(_) => "afs_write_int",
                IrType::Float(FloatWidth::F64) => "afs_write_real64",
                IrType::Float(_) => "afs_write_real",
                IrType::Bool => "afs_write_logical",
                IrType::Ptr(ref inner) => {
                    // Complex expression result: ptr<[f32/f64 x 2]>
                    if is_complex_ty(&ty) {
                        let fw = complex_float_width(&ty);
                        let func = if fw == FloatWidth::F64 {
                            "afs_write_complex_f64"
                        } else {
                            "afs_write_complex_f32"
                        };
                        b.call(FuncRef::External(func.into()), vec![unit, val], IrType::Void);
                        continue;
                    }
                    // Other pointer — likely a string. Use write_string with literal length.
                    let _ = inner; // suppress unused warning
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

fn internal_io_buffer(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    control: &crate::ast::stmt::IoControl,
) -> Option<(ValueId, ValueId)> {
    if control.keyword.as_deref().map(|k| !k.eq_ignore_ascii_case("unit")).unwrap_or(false) {
        return None;
    }

    match &control.value.node {
        Expr::Name { name } => {
            let info = ctx.locals.get(&name.to_lowercase())?;
            if info.char_kind == CharKind::None {
                return None;
            }
            Some(lower_string_expr(b, &ctx.locals, &control.value, ctx.st))
        }
        _ => None,
    }
}

fn lower_internal_write_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    buf_ptr: ValueId,
    buf_len: ValueId,
) {
    let zero = b.const_i64(0);
    let pos = b.alloca(IrType::Int(IntWidth::I64));
    b.store(zero, pos);

    for item in items {
        let is_char = match &item.node {
            Expr::Name { name } => ctx.locals.get(&name.to_lowercase())
                .map(|i| i.char_kind != CharKind::None)
                .unwrap_or(false),
            Expr::FunctionCall { callee, args } => {
                if let Expr::Name { name } = &callee.node {
                    let key = name.to_lowercase();
                    matches!(key.as_str(),
                        "trim" | "adjustl" | "adjustr" | "char")
                        || ctx.locals.get(&key)
                            .map(|i| i.char_kind != CharKind::None && i.dims.is_empty())
                            .unwrap_or(false)
                } else if let Expr::FunctionCall { callee: inner, .. } = &callee.node {
                    // Nested: arr(i)(lo:hi) — substring of char array element.
                    if let Expr::Name { name } = &inner.node {
                        let key = name.to_lowercase();
                        ctx.locals.get(&key)
                            .map(|i| i.char_kind != CharKind::None && (!i.dims.is_empty() || i.allocatable))
                            .unwrap_or(false)
                            && args.iter().any(|a| matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. }))
                    } else { false }
                } else { false }
            }
            _ => false,
        };

        if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
            let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
            b.call(
                FuncRef::External("afs_write_internal_string".into()),
                vec![buf_ptr, buf_len, ptr, len, pos],
                IrType::Void,
            );
            continue;
        }

        let val = lower_expr_tl(b, &ctx.locals, item, ctx.st, ctx.type_layouts);
        let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
        match ty {
            IrType::Int(IntWidth::I128) => {
                b.call(
                    FuncRef::External("afs_write_internal_int128".into()),
                    vec![buf_ptr, buf_len, val, pos],
                    IrType::Void,
                );
            }
            IrType::Int(IntWidth::I64) => {
                b.call(
                    FuncRef::External("afs_write_internal_int64".into()),
                    vec![buf_ptr, buf_len, val, pos],
                    IrType::Void,
                );
            }
            IrType::Int(_) => {
                let i32_val = if matches!(ty, IrType::Int(IntWidth::I32)) {
                    val
                } else {
                    b.int_extend(val, IntWidth::I32, true)
                };
                b.call(
                    FuncRef::External("afs_write_internal_int".into()),
                    vec![buf_ptr, buf_len, i32_val, pos],
                    IrType::Void,
                );
            }
            IrType::Float(FloatWidth::F64) => {
                b.call(
                    FuncRef::External("afs_write_internal_real64".into()),
                    vec![buf_ptr, buf_len, val, pos],
                    IrType::Void,
                );
            }
            IrType::Float(_) => {
                let widened = b.float_extend(val, FloatWidth::F64);
                b.call(
                    FuncRef::External("afs_write_internal_real64".into()),
                    vec![buf_ptr, buf_len, widened, pos],
                    IrType::Void,
                );
            }
            _ => {}
        }
    }
}

fn lower_list_read_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    unit: ValueId,
) {
    let iostat = b.const_i64(0);
    let mode = ReadMode::Unit { unit, iostat };

    for item in items {
        if lower_array_read_item(b, ctx, item, mode) {
            continue;
        }
        let Some((addr, ty)) = lower_read_target_addr(b, ctx, item) else {
            continue;
        };
        let _ = lower_read_into_addr(b, mode, &ty, addr);
    }
}

fn lower_internal_read_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    buf_ptr: ValueId,
    buf_len: ValueId,
) {
    let zero = b.const_i64(0);
    let pos = b.alloca(IrType::Int(IntWidth::I64));
    b.store(zero, pos);
    let iostat = b.alloca(IrType::Int(IntWidth::I32));
    let mode = ReadMode::Internal { buf_ptr, buf_len, pos, iostat };

    for item in items {
        if lower_array_read_item(b, ctx, item, mode) {
            continue;
        }
        let Some((addr, ty)) = lower_read_target_addr(b, ctx, item) else {
            continue;
        };
        let _ = lower_read_into_addr(b, mode, &ty, addr);
    }
}

#[derive(Clone, Copy)]
enum ReadMode {
    Unit {
        unit: ValueId,
        iostat: ValueId,
    },
    Internal {
        buf_ptr: ValueId,
        buf_len: ValueId,
        pos: ValueId,
        iostat: ValueId,
    },
    FormattedUnit {
        unit: ValueId,
        fmt_ptr: ValueId,
        fmt_len: ValueId,
        item_idx: ValueId,
        iostat: ValueId,
    },
    FormattedInternal {
        buf_ptr: ValueId,
        buf_len: ValueId,
        fmt_ptr: ValueId,
        fmt_len: ValueId,
        item_idx: ValueId,
        iostat: ValueId,
    },
}

fn lower_read_into_addr(
    b: &mut FuncBuilder,
    mode: ReadMode,
    ty: &IrType,
    addr: ValueId,
) -> bool {
    match ty {
        IrType::Int(IntWidth::I128) => {
            match mode {
                ReadMode::Unit { unit, iostat } => {
                    b.call(FuncRef::External("afs_read_int128".into()), vec![unit, addr, iostat], IrType::Void);
                }
                ReadMode::Internal { buf_ptr, buf_len, pos, iostat } => {
                    b.call(
                        FuncRef::External("afs_read_internal_int128".into()),
                        vec![buf_ptr, buf_len, pos, addr, iostat],
                        IrType::Void,
                    );
                }
                ReadMode::FormattedUnit { unit, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int128".into()),
                        vec![unit, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
                ReadMode::FormattedInternal { buf_ptr, buf_len, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int128_internal".into()),
                        vec![buf_ptr, buf_len, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
            }
            true
        }
        IrType::Int(IntWidth::I64) => {
            match mode {
                ReadMode::Unit { unit, iostat } => {
                    b.call(FuncRef::External("afs_read_int64".into()), vec![unit, addr, iostat], IrType::Void);
                }
                ReadMode::Internal { buf_ptr, buf_len, pos, iostat } => {
                    b.call(
                        FuncRef::External("afs_read_internal_int64".into()),
                        vec![buf_ptr, buf_len, pos, addr, iostat],
                        IrType::Void,
                    );
                }
                ReadMode::FormattedUnit { unit, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int64".into()),
                        vec![unit, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
                ReadMode::FormattedInternal { buf_ptr, buf_len, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int64_internal".into()),
                        vec![buf_ptr, buf_len, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
            }
            true
        }
        IrType::Int(_) => {
            match mode {
                ReadMode::Unit { unit, iostat } => {
                    b.call(FuncRef::External("afs_read_int".into()), vec![unit, addr, iostat], IrType::Void);
                }
                ReadMode::Internal { buf_ptr, buf_len, pos, iostat } => {
                    b.call(
                        FuncRef::External("afs_read_internal_int".into()),
                        vec![buf_ptr, buf_len, pos, addr, iostat],
                        IrType::Void,
                    );
                }
                ReadMode::FormattedUnit { unit, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int".into()),
                        vec![unit, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
                ReadMode::FormattedInternal { buf_ptr, buf_len, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_int_internal".into()),
                        vec![buf_ptr, buf_len, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
            }
            true
        }
        IrType::Float(FloatWidth::F64) => {
            match mode {
                ReadMode::Unit { unit, iostat } => {
                    b.call(FuncRef::External("afs_read_real64".into()), vec![unit, addr, iostat], IrType::Void);
                }
                ReadMode::Internal { buf_ptr, buf_len, pos, iostat } => {
                    b.call(
                        FuncRef::External("afs_read_internal_real".into()),
                        vec![buf_ptr, buf_len, pos, addr, iostat],
                        IrType::Void,
                    );
                }
                ReadMode::FormattedUnit { unit, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_real".into()),
                        vec![unit, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
                ReadMode::FormattedInternal { buf_ptr, buf_len, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_real_internal".into()),
                        vec![buf_ptr, buf_len, fmt_ptr, fmt_len, current_idx, addr, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                }
            }
            true
        }
        IrType::Float(FloatWidth::F32) => {
            let tmp = b.alloca(IrType::Float(FloatWidth::F64));
            let handled = match mode {
                ReadMode::Unit { unit, iostat } => {
                    b.call(FuncRef::External("afs_read_real".into()), vec![unit, tmp, iostat], IrType::Void);
                    true
                }
                ReadMode::Internal { buf_ptr, buf_len, pos, iostat } => {
                    b.call(
                        FuncRef::External("afs_read_internal_real".into()),
                        vec![buf_ptr, buf_len, pos, tmp, iostat],
                        IrType::Void,
                    );
                    true
                }
                ReadMode::FormattedUnit { unit, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_real".into()),
                        vec![unit, fmt_ptr, fmt_len, current_idx, tmp, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                    true
                }
                ReadMode::FormattedInternal { buf_ptr, buf_len, fmt_ptr, fmt_len, item_idx, iostat } => {
                    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
                    b.call(
                        FuncRef::External("afs_fmt_read_real_internal".into()),
                        vec![buf_ptr, buf_len, fmt_ptr, fmt_len, current_idx, tmp, iostat],
                        IrType::Void,
                    );
                    bump_formatted_read_index(b, item_idx);
                    true
                }
            };
            let wide = b.load_typed(tmp, IrType::Float(FloatWidth::F64));
            let narrow = b.float_trunc(wide, FloatWidth::F32);
            b.store(narrow, addr);
            handled
        }
        _ => false,
    }
}

fn bump_formatted_read_index(b: &mut FuncBuilder, item_idx: ValueId) {
    let current_idx = b.load_typed(item_idx, IrType::Int(IntWidth::I64));
    let one = b.const_i64(1);
    let next_idx = b.iadd(current_idx, one);
    b.store(next_idx, item_idx);
}

fn lower_array_read_item(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    item: &crate::ast::expr::SpannedExpr,
    mode: ReadMode,
) -> bool {
    match &item.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            let Some(info) = ctx.locals.get(&key).cloned() else {
                return false;
            };
            if info.dims.is_empty() && !info.allocatable {
                return false;
            }
            lower_whole_array_read(b, &info, mode);
            true
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return false;
            };
            let key = name.to_lowercase();
            let Some(info) = ctx.locals.get(&key).cloned() else {
                return false;
            };
            if info.dims.is_empty() && !info.allocatable {
                return false;
            }
            let has_range = args.iter().any(|arg| {
                matches!(arg.value, crate::ast::expr::SectionSubscript::Range { .. })
            });
            if has_range && info.allocatable {
                lower_alloc_section_read(b, ctx, &info, args, mode);
                true
            } else if has_range && args.len() == 1 {
                lower_1d_slice_read(b, ctx, &info, &args[0], mode);
                true
            } else if has_range && args.len() > 1 && !info.allocatable {
                lower_section_read_nd(b, ctx, &info, args, mode);
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn lower_read_target_addr(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    item: &crate::ast::expr::SpannedExpr,
) -> Option<(ValueId, IrType)> {
    match &item.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            let info = ctx.locals.get(&key)?;
            if !info.dims.is_empty() || info.allocatable {
                return None;
            }
            let addr = if info.by_ref { b.load(info.addr) } else { info.addr };
            Some((addr, info.ty.clone()))
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let key = name.to_lowercase();
            let info = ctx.locals.get(&key)?;
            if info.dims.is_empty() && !info.allocatable {
                return None;
            }
            if args.iter().any(|arg| {
                !matches!(arg.value, crate::ast::expr::SectionSubscript::Element(_))
            }) {
                return None;
            }
            let idx64 = compute_flat_elem_offset(b, &ctx.locals, info, args, ctx.st);
            let base = array_base_addr(b, info);
            let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
            Some((elem_ptr, info.ty.clone()))
        }
        Expr::ComponentAccess { base, component } => {
            let (base_addr, type_name) =
                resolve_component_base(b, &ctx.locals, base, ctx.type_layouts)?;
            let layout = ctx.type_layouts.get(&type_name)?;
            let field = layout.field(component)?;
            if matches!(&field.type_info, crate::sema::symtab::TypeInfo::Derived(_)) {
                return None;
            }
            let offset = b.const_i64(field.offset as i64);
            let field_ptr = b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));
            Some((field_ptr, type_info_to_ir_type(&field.type_info)))
        }
        _ => None,
    }
}

fn lower_formatted_internal_read_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    buf_ptr: ValueId,
    buf_len: ValueId,
    fmt_ptr: ValueId,
    fmt_len: ValueId,
) {
    let item_idx = b.alloca(IrType::Int(IntWidth::I64));
    let iostat = b.alloca(IrType::Int(IntWidth::I32));
    let zero = b.const_i64(0);
    b.store(zero, item_idx);
    let mode = ReadMode::FormattedInternal {
        buf_ptr,
        buf_len,
        fmt_ptr,
        fmt_len,
        item_idx,
        iostat,
    };

    for item in items {
        if lower_array_read_item(b, ctx, item, mode) {
            continue;
        }
        let Some((addr, ty)) = lower_read_target_addr(b, ctx, item) else {
            continue;
        };
        let _ = lower_read_into_addr(b, mode, &ty, addr);
    }
}

fn lower_formatted_read_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    unit: ValueId,
    fmt_ptr: ValueId,
    fmt_len: ValueId,
) {
    let item_idx = b.alloca(IrType::Int(IntWidth::I64));
    let iostat = b.alloca(IrType::Int(IntWidth::I32));
    let zero = b.const_i64(0);
    b.store(zero, item_idx);
    let mode = ReadMode::FormattedUnit {
        unit,
        fmt_ptr,
        fmt_len,
        item_idx,
        iostat,
    };

    for item in items {
        if lower_array_read_item(b, ctx, item, mode) {
            continue;
        }
        let Some((addr, ty)) = lower_read_target_addr(b, ctx, item) else {
            continue;
        };
        let _ = lower_read_into_addr(b, mode, &ty, addr);
    }
}

/// Push a single I/O item value for formatted output via afs_fmt_push_*.
fn lower_fmt_push(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    item: &crate::ast::expr::SpannedExpr,
) {
    let is_char = match &item.node {
        Expr::Name { name } => ctx.locals.get(&name.to_lowercase())
            .map(|i| i.char_kind != CharKind::None)
            .unwrap_or(false),
        Expr::FunctionCall { callee, .. } => {
            if let Expr::Name { name } = &callee.node {
                matches!(name.to_lowercase().as_str(),
                    "trim" | "adjustl" | "adjustr" | "char")
            } else { false }
        }
        _ => false,
    };

    if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
        let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
        b.call(FuncRef::External("afs_fmt_push_string".into()), vec![ptr, len], IrType::Void);
    } else {
        let val = lower_expr(b, &ctx.locals, item, ctx.st);
        let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
        match &ty {
            IrType::Int(IntWidth::I128) => {
                let slot = b.alloca(IrType::Int(IntWidth::I128));
                b.store(val, slot);
                b.call(FuncRef::External("afs_fmt_push_int128".into()), vec![slot], IrType::Void);
            }
            IrType::Int(IntWidth::I64) => {
                b.call(FuncRef::External("afs_fmt_push_int".into()), vec![val], IrType::Void);
            }
            IrType::Int(_) => {
                // Widen i32 to i64 for the push API.
                let widened = b.int_extend(val, IntWidth::I64, true);
                b.call(FuncRef::External("afs_fmt_push_int".into()), vec![widened], IrType::Void);
            }
            IrType::Float(FloatWidth::F32) => {
                // afs_fmt_push_real takes f64; explicitly widen f32 → f64.
                // AArch64 does NOT auto-promote floats across the call boundary.
                let widened = b.float_extend(val, FloatWidth::F64);
                b.call(FuncRef::External("afs_fmt_push_real".into()), vec![widened], IrType::Void);
            }
            IrType::Float(_) => {
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

/// Lower a 1-D slice write item: `print *, a(lo:hi[:step])`.
/// Iterates the declared range and calls the per-element write
/// helper. Sections with a rank > 1 and non-lower-dim subscripts
/// are not yet supported and fall through to the existing
/// section-descriptor path, which may not format nicely.
///
/// Missing bounds default to the array's declared extents:
///   `a(:)`   → full range
///   `a(lo:)` → lo to end
///   `a(:hi)` → start to hi
fn lower_1d_slice_write(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    info: &LocalInfo,
    arg: &crate::ast::expr::Argument,
    unit: ValueId,
) {
    let (start_e, end_e, stride_e) = match &arg.value {
        crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
            (start.as_ref(), end.as_ref(), stride.as_ref())
        }
        _ => return,
    };

    // Default to the declared bounds of dimension 0.
    let (decl_lo, decl_ext) = info.dims.first().copied().unwrap_or((1, 0));
    let decl_hi = decl_lo + decl_ext - 1;

    let start_val = match start_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(decl_lo as i32),
    };
    let end_val = match end_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(decl_hi as i32),
    };
    let stride_val = match stride_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(1),
    };

    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);
    let writer = match &info.ty {
        IrType::Int(IntWidth::I128) => "afs_write_int128",
        IrType::Int(IntWidth::I64) => "afs_write_int64",
        IrType::Int(_) => "afs_write_int",
        IrType::Float(FloatWidth::F64) => "afs_write_real64",
        IrType::Float(_) => "afs_write_real",
        IrType::Bool => "afs_write_logical",
        _ => "afs_write_int",
    };

    // `i` counter, starts at the slice's first index.
    let i_addr = b.alloca(IrType::Int(IntWidth::I32));
    b.store(start_val, i_addr);

    let bb_check = b.create_block("slice_write_check");
    let bb_body  = b.create_block("slice_write_body");
    let bb_exit  = b.create_block("slice_write_exit");
    b.branch(bb_check, vec![]);

    // Sign-of-stride handling, mirroring the regular DO lowerer.
    // For ascending stride, exit when `i > end`; for descending,
    // exit when `i < end`. Audit BLOCKING-2: the previous version
    // hardcoded `Gt`, so `print *, a(5:1:-1)` exited on the very
    // first iteration with no elements written.
    b.set_block(bb_check);
    let i = b.load(i_addr);
    let const_stride = stride_e.and_then(eval_const_int);
    if let Some(sv) = const_stride {
        let done_op = if sv < 0 { CmpOp::Lt } else { CmpOp::Gt };
        let done = b.icmp(done_op, i, end_val);
        b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);
    } else {
        // Runtime stride: branch on sign at the check site.
        let zero = b.const_i32(0);
        let stride_neg = b.icmp(CmpOp::Lt, stride_val, zero);
        let bb_neg = b.create_block("slice_write_neg_check");
        let bb_pos = b.create_block("slice_write_pos_check");
        b.cond_branch(stride_neg, bb_neg, vec![], bb_pos, vec![]);

        b.set_block(bb_neg);
        let done_neg = b.icmp(CmpOp::Lt, i, end_val);
        b.cond_branch(done_neg, bb_exit, vec![], bb_body, vec![]);

        b.set_block(bb_pos);
        let done_pos = b.icmp(CmpOp::Gt, i, end_val);
        b.cond_branch(done_pos, bb_exit, vec![], bb_body, vec![]);
    }

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    // Translate declared-index `i` → byte offset into base:
    //   (i - decl_lo) * elem_bytes
    let lo_const = b.const_i32(decl_lo as i32);
    let zero_based = b.isub(i_val, lo_const);
    let zero_based64 = widen_idx_to_i64(b, zero_based);
    let step = b.const_i64(elem_bytes);
    let byte_off = b.imul(zero_based64, step);
    let p = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
    let elem = b.load_typed(p, info.ty.clone());
    b.call(FuncRef::External(writer.into()), vec![unit, elem], IrType::Void);
    let next = b.iadd(i_val, stride_val);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn lower_1d_slice_read(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    info: &LocalInfo,
    arg: &crate::ast::expr::Argument,
    mode: ReadMode,
) {
    let (start_e, end_e, stride_e) = match &arg.value {
        crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
            (start.as_ref(), end.as_ref(), stride.as_ref())
        }
        _ => return,
    };

    let (decl_lo, decl_ext) = info.dims.first().copied().unwrap_or((1, 0));
    let decl_hi = decl_lo + decl_ext - 1;

    let start_val = match start_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(decl_lo as i32),
    };
    let end_val = match end_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(decl_hi as i32),
    };
    let stride_val = match stride_e {
        Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
        None => b.const_i32(1),
    };

    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);

    let i_addr = b.alloca(IrType::Int(IntWidth::I32));
    b.store(start_val, i_addr);

    let bb_check = b.create_block("slice_read_check");
    let bb_body = b.create_block("slice_read_body");
    let bb_exit = b.create_block("slice_read_exit");
    b.branch(bb_check, vec![]);

    b.set_block(bb_check);
    let i = b.load(i_addr);
    let const_stride = stride_e.and_then(eval_const_int);
    if let Some(sv) = const_stride {
        let done_op = if sv < 0 { CmpOp::Lt } else { CmpOp::Gt };
        let done = b.icmp(done_op, i, end_val);
        b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);
    } else {
        let zero = b.const_i32(0);
        let stride_neg = b.icmp(CmpOp::Lt, stride_val, zero);
        let bb_neg = b.create_block("slice_read_neg_check");
        let bb_pos = b.create_block("slice_read_pos_check");
        b.cond_branch(stride_neg, bb_neg, vec![], bb_pos, vec![]);

        b.set_block(bb_neg);
        let done_neg = b.icmp(CmpOp::Lt, i, end_val);
        b.cond_branch(done_neg, bb_exit, vec![], bb_body, vec![]);

        b.set_block(bb_pos);
        let done_pos = b.icmp(CmpOp::Gt, i, end_val);
        b.cond_branch(done_pos, bb_exit, vec![], bb_body, vec![]);
    }

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    let lo_const = b.const_i32(decl_lo as i32);
    let zero_based = b.isub(i_val, lo_const);
    let zero_based64 = widen_idx_to_i64(b, zero_based);
    let step = b.const_i64(elem_bytes);
    let byte_off = b.imul(zero_based64, step);
    let p = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
    let _ = lower_read_into_addr(b, mode, &info.ty, p);
    let next = b.iadd(i_val, stride_val);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn lower_section_read_nd(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    mode: ReadMode,
) {
    use crate::ast::expr::SectionSubscript;

    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);

    struct DimSlice {
        counter: ValueId,
        start_val: ValueId,
        end_val: ValueId,
        stride_val: ValueId,
        const_stride: Option<i64>,
        decl_lo: i64,
        cum_stride: i64,
    }

    let mut dims: Vec<DimSlice> = Vec::with_capacity(args.len());
    let mut cum_stride: i64 = 1;
    for (dim_idx, arg) in args.iter().enumerate() {
        let (decl_lo, decl_ext) = info.dims.get(dim_idx).copied().unwrap_or((1, 0));
        let decl_hi = decl_lo + decl_ext - 1;

        let counter = b.alloca(IrType::Int(IntWidth::I32));
        let (start_val, end_val, stride_val, const_stride) = match &arg.value {
            SectionSubscript::Range { start, end, stride } => {
                let start_v = match start {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(decl_lo as i32),
                };
                let end_v = match end {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(decl_hi as i32),
                };
                let stride_v = match stride {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(1),
                };
                let cs = stride.as_ref().and_then(eval_const_int);
                (start_v, end_v, stride_v, cs)
            }
            SectionSubscript::Element(e) => {
                let v = lower_expr(b, &ctx.locals, e, ctx.st);
                (v, v, b.const_i32(1), Some(1))
            }
        };
        b.store(start_val, counter);
        dims.push(DimSlice {
            counter,
            start_val,
            end_val,
            stride_val,
            const_stride,
            decl_lo,
            cum_stride,
        });
        cum_stride *= decl_ext.max(1);
    }

    let n = dims.len();
    let mut checks: Vec<BlockId> = Vec::with_capacity(n);
    let mut bodies: Vec<BlockId> = Vec::with_capacity(n);
    let mut incrs: Vec<BlockId> = Vec::with_capacity(n);
    let mut exits: Vec<BlockId> = Vec::with_capacity(n);
    for d in 0..n {
        checks.push(b.create_block(&format!("read_sec_check_d{}", d)));
        bodies.push(b.create_block(&format!("read_sec_body_d{}", d)));
        incrs.push(b.create_block(&format!("read_sec_incr_d{}", d)));
        exits.push(b.create_block(&format!("read_sec_exit_d{}", d)));
    }

    let outer = n - 1;
    b.branch(checks[outer], vec![]);

    for d_rev in 0..n {
        let d = n - 1 - d_rev;

        b.set_block(checks[d]);
        let cur = b.load(dims[d].counter);
        if let Some(sv) = dims[d].const_stride {
            let done_op = if sv < 0 { CmpOp::Lt } else { CmpOp::Gt };
            let done = b.icmp(done_op, cur, dims[d].end_val);
            b.cond_branch(done, exits[d], vec![], bodies[d], vec![]);
        } else {
            let zero = b.const_i32(0);
            let stride_neg = b.icmp(CmpOp::Lt, dims[d].stride_val, zero);
            let bb_neg = b.create_block(&format!("read_sec_neg_d{}", d));
            let bb_pos = b.create_block(&format!("read_sec_pos_d{}", d));
            b.cond_branch(stride_neg, bb_neg, vec![], bb_pos, vec![]);

            b.set_block(bb_neg);
            let done_neg = b.icmp(CmpOp::Lt, cur, dims[d].end_val);
            b.cond_branch(done_neg, exits[d], vec![], bodies[d], vec![]);

            b.set_block(bb_pos);
            let done_pos = b.icmp(CmpOp::Gt, cur, dims[d].end_val);
            b.cond_branch(done_pos, exits[d], vec![], bodies[d], vec![]);
        }

        b.set_block(bodies[d]);
        if d == 0 {
            let mut byte_offset: Option<ValueId> = None;
            let dim_data: Vec<(ValueId, i64, i64)> = dims
                .iter()
                .map(|dim| (dim.counter, dim.decl_lo, dim.cum_stride))
                .collect();
            for (counter, decl_lo, cum_stride_d) in dim_data {
                let cnt = b.load(counter);
                let lo_const = b.const_i32(decl_lo as i32);
                let zero_based = b.isub(cnt, lo_const);
                let zero_based64 = widen_idx_to_i64(b, zero_based);
                let stride_const = b.const_i64(cum_stride_d * elem_bytes);
                let term = b.imul(zero_based64, stride_const);
                byte_offset = Some(match byte_offset {
                    Some(prev) => b.iadd(prev, term),
                    None => term,
                });
            }
            let off = byte_offset.unwrap_or_else(|| b.const_i64(0));
            let ptr = b.gep(base, vec![off], IrType::Int(IntWidth::I8));
            let _ = lower_read_into_addr(b, mode, &info.ty, ptr);
            b.branch(incrs[0], vec![]);
        } else {
            b.store(dims[d - 1].start_val, dims[d - 1].counter);
            b.branch(checks[d - 1], vec![]);
        }

        b.set_block(incrs[d]);
        let cur2 = b.load(dims[d].counter);
        let next = b.iadd(cur2, dims[d].stride_val);
        b.store(next, dims[d].counter);
        b.branch(checks[d], vec![]);

        b.set_block(exits[d]);
        if d < n - 1 {
            b.branch(incrs[d + 1], vec![]);
        }
    }

    b.set_block(exits[outer]);
}

fn load_array_desc_i64_field(
    b: &mut FuncBuilder,
    desc: ValueId,
    offset: i64,
) -> ValueId {
    let off = b.const_i64(offset);
    let ptr = b.gep(desc, vec![off], IrType::Int(IntWidth::I8));
    b.load_typed(ptr, IrType::Int(IntWidth::I64))
}

fn lower_alloc_section_read(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    mode: ReadMode,
) {
    use crate::ast::expr::SectionSubscript;

    struct DimSlice {
        counter: ValueId,
        start_val: ValueId,
        end_val: ValueId,
        stride_val: ValueId,
        const_stride: Option<i64>,
        lower_bound: ValueId,
        mem_stride: ValueId,
        cum_extent: ValueId,
    }

    let elem_bytes = ir_scalar_byte_size(&info.ty);
    let base = array_base_addr(b, info);
    let one64 = b.const_i64(1);
    let zero64 = b.const_i64(0);

    let mut dims: Vec<DimSlice> = Vec::with_capacity(args.len());
    let mut cum_extent = one64;
    for (dim_idx, arg) in args.iter().enumerate() {
        let dim_base = 24 + (dim_idx as i64) * 24;
        let lo = load_array_desc_i64_field(b, info.addr, dim_base);
        let up = load_array_desc_i64_field(b, info.addr, dim_base + 8);
        let mem_stride = load_array_desc_i64_field(b, info.addr, dim_base + 16);
        let span = b.isub(up, lo);
        let extent_raw = b.iadd(span, one64);
        let is_empty = b.icmp(CmpOp::Lt, up, lo);
        let extent = b.select(is_empty, zero64, extent_raw);
        let (start_val, end_val, stride_val, const_stride) = match &arg.value {
            SectionSubscript::Range { start, end, stride } => {
                let start_v = match start {
                    Some(e) => {
                        let raw = lower_expr(b, &ctx.locals, e, ctx.st);
                        widen_idx_to_i64(b, raw)
                    }
                    None => lo,
                };
                let end_v = match end {
                    Some(e) => {
                        let raw = lower_expr(b, &ctx.locals, e, ctx.st);
                        widen_idx_to_i64(b, raw)
                    }
                    None => up,
                };
                let stride_v = match stride {
                    Some(e) => {
                        let raw = lower_expr(b, &ctx.locals, e, ctx.st);
                        widen_idx_to_i64(b, raw)
                    }
                    None => one64,
                };
                let cs = stride.as_ref().and_then(eval_const_int);
                (start_v, end_v, stride_v, cs)
            }
            SectionSubscript::Element(e) => {
                let raw = lower_expr(b, &ctx.locals, e, ctx.st);
                let val = widen_idx_to_i64(b, raw);
                (val, val, one64, Some(1))
            }
        };
        let counter = b.alloca(IrType::Int(IntWidth::I64));
        b.store(start_val, counter);
        dims.push(DimSlice {
            counter,
            start_val,
            end_val,
            stride_val,
            const_stride,
            lower_bound: lo,
            mem_stride,
            cum_extent,
        });
        cum_extent = b.imul(cum_extent, extent);
    }

    let n = dims.len();
    let mut checks: Vec<BlockId> = Vec::with_capacity(n);
    let mut bodies: Vec<BlockId> = Vec::with_capacity(n);
    let mut incrs: Vec<BlockId> = Vec::with_capacity(n);
    let mut exits: Vec<BlockId> = Vec::with_capacity(n);
    for d in 0..n {
        checks.push(b.create_block(&format!("read_desc_check_d{}", d)));
        bodies.push(b.create_block(&format!("read_desc_body_d{}", d)));
        incrs.push(b.create_block(&format!("read_desc_incr_d{}", d)));
        exits.push(b.create_block(&format!("read_desc_exit_d{}", d)));
    }

    let outer = n - 1;
    b.branch(checks[outer], vec![]);

    for d_rev in 0..n {
        let d = n - 1 - d_rev;

        b.set_block(checks[d]);
        let cur = b.load(dims[d].counter);
        if let Some(sv) = dims[d].const_stride {
            let done_op = if sv < 0 { CmpOp::Lt } else { CmpOp::Gt };
            let done = b.icmp(done_op, cur, dims[d].end_val);
            b.cond_branch(done, exits[d], vec![], bodies[d], vec![]);
        } else {
            let stride_neg = b.icmp(CmpOp::Lt, dims[d].stride_val, zero64);
            let bb_neg = b.create_block(&format!("read_alloc_neg_d{}", d));
            let bb_pos = b.create_block(&format!("read_alloc_pos_d{}", d));
            b.cond_branch(stride_neg, bb_neg, vec![], bb_pos, vec![]);

            b.set_block(bb_neg);
            let done_neg = b.icmp(CmpOp::Lt, cur, dims[d].end_val);
            b.cond_branch(done_neg, exits[d], vec![], bodies[d], vec![]);

            b.set_block(bb_pos);
            let done_pos = b.icmp(CmpOp::Gt, cur, dims[d].end_val);
            b.cond_branch(done_pos, exits[d], vec![], bodies[d], vec![]);
        }

        b.set_block(bodies[d]);
        if d == 0 {
            let dim_data: Vec<(ValueId, ValueId, ValueId, ValueId)> = dims
                .iter()
                .map(|dim| (dim.counter, dim.lower_bound, dim.mem_stride, dim.cum_extent))
                .collect();
            let mut elem_offset: Option<ValueId> = None;
            for (counter, lower_bound, mem_stride, cum_extent_d) in dim_data {
                let cnt = b.load(counter);
                let adjusted = b.isub(cnt, lower_bound);
                let scaled = b.imul(adjusted, cum_extent_d);
                let term = b.imul(scaled, mem_stride);
                elem_offset = Some(match elem_offset {
                    Some(prev) => b.iadd(prev, term),
                    None => term,
                });
            }
            let off_elems = elem_offset.unwrap_or_else(|| b.const_i64(0));
            let elem_bytes_v = b.const_i64(elem_bytes);
            let byte_off = b.imul(off_elems, elem_bytes_v);
            let ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
            let _ = lower_read_into_addr(b, mode, &info.ty, ptr);
            b.branch(incrs[0], vec![]);
        } else {
            b.store(dims[d - 1].start_val, dims[d - 1].counter);
            b.branch(checks[d - 1], vec![]);
        }

        b.set_block(incrs[d]);
        let cur2 = b.load(dims[d].counter);
        let next = b.iadd(cur2, dims[d].stride_val);
        b.store(next, dims[d].counter);
        b.branch(checks[d], vec![]);

        b.set_block(exits[d]);
        if d < n - 1 {
            b.branch(incrs[d + 1], vec![]);
        }
    }

    b.set_block(exits[outer]);
}

/// Lower an N-dimensional array section write item, e.g.
/// `print *, m(:, 1)` or `print *, m(2:3, 1:2)`. Generates one
/// nested loop per dimension, innermost = dim 0 (Fortran column-
/// major iteration order), and at the leaf computes the flat
/// byte offset into the array's base.
///
/// Element subscripts (`m(:, 1)`) collapse to a single iteration
/// at the fixed value. Range subscripts iterate from start to
/// end with the given stride (defaults: declared bounds, stride
/// 1). Stride sign is honored both at compile time and at runtime.
///
/// Audit CRITICAL-3: multi-dim slice prints used to mis-dispatch
/// through afs_create_section on a bare stack pointer and crash
/// at runtime reading 384 bytes of garbage as a descriptor.
fn lower_section_write_nd(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    unit: ValueId,
) {
    use crate::ast::expr::SectionSubscript;

    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);
    let writer = match &info.ty {
        IrType::Int(IntWidth::I128) => "afs_write_int128",
        IrType::Int(IntWidth::I64) => "afs_write_int64",
        IrType::Int(_) => "afs_write_int",
        IrType::Float(FloatWidth::F64) => "afs_write_real64",
        IrType::Float(_) => "afs_write_real",
        IrType::Bool => "afs_write_logical",
        _ => "afs_write_int",
    };

    // For each dimension we need: a runtime counter alloca plus
    // its start/end/stride values, the declared lower bound (for
    // base-relative offset arithmetic), and the cumulative stride
    // for column-major flat-offset computation. start_val is
    // saved so non-innermost loop bodies can RE-init the inner
    // counter on each outer iteration.
    struct DimSlice {
        counter: ValueId,
        start_val: ValueId,
        end_val: ValueId,
        stride_val: ValueId,
        const_stride: Option<i64>,
        decl_lo: i64,
        cum_stride: i64,
    }

    let mut dims: Vec<DimSlice> = Vec::with_capacity(args.len());
    let mut cum_stride: i64 = 1;
    for (dim_idx, arg) in args.iter().enumerate() {
        let (decl_lo, decl_ext) = info.dims.get(dim_idx).copied().unwrap_or((1, 0));
        let decl_hi = decl_lo + decl_ext - 1;

        let counter = b.alloca(IrType::Int(IntWidth::I32));
        let (start_val, end_val, stride_val, const_stride) = match &arg.value {
            SectionSubscript::Range { start, end, stride } => {
                let start_v = match start {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(decl_lo as i32),
                };
                let end_v = match end {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(decl_hi as i32),
                };
                let stride_v = match stride {
                    Some(e) => lower_expr(b, &ctx.locals, e, ctx.st),
                    None => b.const_i32(1),
                };
                let cs = stride.as_ref().and_then(eval_const_int);
                (start_v, end_v, stride_v, cs)
            }
            SectionSubscript::Element(e) => {
                let v = lower_expr(b, &ctx.locals, e, ctx.st);
                // Single-element dimension: start == end, stride 1.
                (v, v, b.const_i32(1), Some(1))
            }
        };
        b.store(start_val, counter);
        dims.push(DimSlice {
            counter, start_val, end_val, stride_val, const_stride,
            decl_lo, cum_stride,
        });
        cum_stride *= decl_ext.max(1);
    }

    // Build nested check/body/exit blocks, OUTERMOST first (last
    // dim) — we want innermost = dim 0 for column-major iteration.
    // Layout per dimension d (counting from outermost):
    //   check_d → body_d? exit_d
    //   body_d:
    //     [if d > 0] init counter[d-1] = start[d-1]; branch check_{d-1}
    //     [if d == 0] compute offset, GEP, write, branch incr_0
    //   incr_d: counter[d] += stride[d]; branch check_d
    //   exit_d
    let n = dims.len();
    let mut checks: Vec<BlockId> = Vec::with_capacity(n);
    let mut bodies: Vec<BlockId> = Vec::with_capacity(n);
    let mut incrs: Vec<BlockId> = Vec::with_capacity(n);
    let mut exits: Vec<BlockId> = Vec::with_capacity(n);
    for d in 0..n {
        checks.push(b.create_block(&format!("sec_check_d{}", d)));
        bodies.push(b.create_block(&format!("sec_body_d{}", d)));
        incrs.push(b.create_block(&format!("sec_incr_d{}", d)));
        exits.push(b.create_block(&format!("sec_exit_d{}", d)));
    }

    // Enter the outermost loop. Walking from outermost (n-1) to
    // innermost (0) means index n-1 is the LAST in the dims vec.
    let outer = n - 1;
    b.branch(checks[outer], vec![]);

    // Emit each dimension's check/incr/exit. Body chains down to
    // the next inner dim (or to the leaf computation at d == 0).
    for d_rev in 0..n {
        let d = n - 1 - d_rev; // outermost first

        // Check block: load counter, compare against end with the
        // appropriate cmp op (sign of stride).
        b.set_block(checks[d]);
        let cur = b.load(dims[d].counter);
        if let Some(sv) = dims[d].const_stride {
            let done_op = if sv < 0 { CmpOp::Lt } else { CmpOp::Gt };
            let done = b.icmp(done_op, cur, dims[d].end_val);
            b.cond_branch(done, exits[d], vec![], bodies[d], vec![]);
        } else {
            let zero = b.const_i32(0);
            let stride_neg = b.icmp(CmpOp::Lt, dims[d].stride_val, zero);
            let bb_neg = b.create_block(&format!("sec_neg_d{}", d));
            let bb_pos = b.create_block(&format!("sec_pos_d{}", d));
            b.cond_branch(stride_neg, bb_neg, vec![], bb_pos, vec![]);

            b.set_block(bb_neg);
            let done_neg = b.icmp(CmpOp::Lt, cur, dims[d].end_val);
            b.cond_branch(done_neg, exits[d], vec![], bodies[d], vec![]);

            b.set_block(bb_pos);
            let done_pos = b.icmp(CmpOp::Gt, cur, dims[d].end_val);
            b.cond_branch(done_pos, exits[d], vec![], bodies[d], vec![]);
        }

        // Body block. If we're at the innermost dim, compute the
        // offset and emit the load+write. Otherwise, init the
        // next-inner dim's counter and branch to its check.
        b.set_block(bodies[d]);
        if d == 0 {
            // Innermost: compute flat offset = sum over all dims of
            // (counter - decl_lo) * cum_stride * elem_bytes.
            let mut byte_offset: Option<ValueId> = None;
            // Borrow `dims` immutably while iterating it; the loop
            // body needs &mut b so we collect the per-dim values
            // first, then emit the IR for the sum afterwards.
            let dim_data: Vec<(ValueId, i64, i64)> = dims.iter()
                .map(|d| (d.counter, d.decl_lo, d.cum_stride))
                .collect();
            for (counter, decl_lo, cum_stride_d) in dim_data {
                let cnt = b.load(counter);
                let lo_const = b.const_i32(decl_lo as i32);
                let zero_based = b.isub(cnt, lo_const);
                let zero_based64 = widen_idx_to_i64(b, zero_based);
                let stride_const = b.const_i64(cum_stride_d * elem_bytes);
                let term = b.imul(zero_based64, stride_const);
                byte_offset = Some(match byte_offset {
                    Some(prev) => b.iadd(prev, term),
                    None => term,
                });
            }
            let off = byte_offset.unwrap_or_else(|| b.const_i64(0));
            let p = b.gep(base, vec![off], IrType::Int(IntWidth::I8));
            let elem = b.load_typed(p, info.ty.clone());
            b.call(FuncRef::External(writer.into()), vec![unit, elem], IrType::Void);
            b.branch(incrs[0], vec![]);
        } else {
            // Not innermost: re-init the next-inner dim's counter
            // to its start value (RESET on each outer iteration),
            // then branch to its check block.
            b.store(dims[d - 1].start_val, dims[d - 1].counter);
            b.branch(checks[d - 1], vec![]);
        }

        // Increment block: counter += stride; branch back to check.
        b.set_block(incrs[d]);
        let cur2 = b.load(dims[d].counter);
        let next = b.iadd(cur2, dims[d].stride_val);
        b.store(next, dims[d].counter);
        b.branch(checks[d], vec![]);

        // Exit block: continue out to next-outer increment, or
        // fall through past everything if this was the outermost.
        b.set_block(exits[d]);
        if d < n - 1 {
            b.branch(incrs[d + 1], vec![]);
        }
        // If d == n-1 (outermost exit), the caller of this helper
        // continues emitting after exits[outer]. We leave the
        // current block set to exits[outer] below.
    }

    // The final current block must be exits[outer] so subsequent
    // statement lowering continues after the section loop.
    b.set_block(exits[outer]);
}

/// Lower a whole-array write item: iterate every element of the
/// array and call the per-element write helper. Used by `print *,
/// arr` and equivalent forms. Without this the array's base
/// pointer leaks into the Ptr<_> arm of the scalar write
/// dispatcher and gets mis-routed to afs_write_string.
fn lower_whole_array_write(
    b: &mut FuncBuilder,
    _ctx: &mut LowerCtx,
    info: &LocalInfo,
    unit: ValueId,
) {
    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);
    let writer = match &info.ty {
        IrType::Int(IntWidth::I128) => "afs_write_int128",
        IrType::Int(IntWidth::I64) => "afs_write_int64",
        IrType::Int(_) => "afs_write_int",
        IrType::Float(FloatWidth::F64) => "afs_write_real64",
        IrType::Float(_) => "afs_write_real",
        IrType::Bool => "afs_write_logical",
        _ => "afs_write_int",
    };

    // Compile-time-known size for stack arrays; runtime descriptor
    // call for allocatables.
    let n = array_total_elems_value(b, info);

    // Stack-allocated loop counter, like lower_array_assign.
    let i_addr = b.alloca(IrType::Int(IntWidth::I64));
    let zero = b.const_i64(0);
    b.store(zero, i_addr);

    let bb_check = b.create_block("write_arr_check");
    let bb_body  = b.create_block("write_arr_body");
    let bb_exit  = b.create_block("write_arr_exit");
    b.branch(bb_check, vec![]);

    b.set_block(bb_check);
    let i = b.load(i_addr);
    let done = b.icmp(CmpOp::Ge, i, n);
    b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    let elem_bytes_v = b.const_i64(elem_bytes);
    let byte_off = b.imul(i_val, elem_bytes_v);
    let ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
    let elem = b.load_typed(ptr, info.ty.clone());
    b.call(FuncRef::External(writer.into()), vec![unit, elem], IrType::Void);
    let one = b.const_i64(1);
    let next = b.iadd(i_val, one);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn lower_whole_array_read(
    b: &mut FuncBuilder,
    info: &LocalInfo,
    mode: ReadMode,
) {
    let base = array_base_addr(b, info);
    let elem_bytes = ir_scalar_byte_size(&info.ty);
    let n = array_total_elems_value(b, info);

    let i_addr = b.alloca(IrType::Int(IntWidth::I64));
    let zero = b.const_i64(0);
    b.store(zero, i_addr);

    let bb_check = b.create_block("read_arr_check");
    let bb_body = b.create_block("read_arr_body");
    let bb_exit = b.create_block("read_arr_exit");
    b.branch(bb_check, vec![]);

    b.set_block(bb_check);
    let i = b.load(i_addr);
    let done = b.icmp(CmpOp::Ge, i, n);
    b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    let elem_bytes_v = b.const_i64(elem_bytes);
    let byte_off = b.imul(i_val, elem_bytes_v);
    let ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
    let _ = lower_read_into_addr(b, mode, &info.ty, ptr);
    let one = b.const_i64(1);
    let next = b.iadd(i_val, one);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn local_uses_array_descriptor(info: &LocalInfo) -> bool {
    info.allocatable || info.descriptor_arg
}

fn array_descriptor_addr(b: &mut FuncBuilder, info: &LocalInfo) -> ValueId {
    if info.allocatable {
        info.addr
    } else if info.descriptor_arg {
        b.load(info.addr)
    } else {
        info.addr
    }
}

fn store_byte_aggregate_field(
    b: &mut FuncBuilder,
    base: ValueId,
    offset: i64,
    field_ty: IrType,
    val: ValueId,
) {
    let field_bytes = field_ty.size_bytes() as i64;
    debug_assert!(field_bytes > 0 && offset % field_bytes == 0);
    let slot = b.const_i64(offset / field_bytes);
    let ptr = b.gep(base, vec![slot], field_ty.clone());
    let stored = match field_ty {
        IrType::Int(_) | IrType::Float(_) | IrType::Bool => coerce_to_type(b, val, &field_ty),
        _ => val,
    };
    b.store(stored, ptr);
}

fn array_data_ptr_for_call(b: &mut FuncBuilder, info: &LocalInfo) -> ValueId {
    if local_uses_array_descriptor(info) {
        let desc = array_descriptor_addr(b, info);
        b.load_typed(desc, IrType::Ptr(Box::new(info.ty.clone())))
    } else if info.by_ref {
        b.load(info.addr)
    } else if !info.dims.is_empty() {
        let zero = b.const_i64(0);
        b.gep(info.addr, vec![zero], info.ty.clone())
    } else {
        info.addr
    }
}

fn materialize_array_descriptor_for_info(
    b: &mut FuncBuilder,
    info: &LocalInfo,
) -> ValueId {
    let desc = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384));
    let zero32 = b.const_i32(0);
    let sz384 = b.const_i64(384);
    b.call(
        FuncRef::External("memset".into()),
        vec![desc, zero32, sz384],
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
    );

    let base_ptr = array_data_ptr_for_call(b, info);
    store_byte_aggregate_field(
        b,
        desc,
        0,
        IrType::Ptr(Box::new(info.ty.clone())),
        base_ptr,
    );
    let elem_size = b.const_i64(ir_scalar_byte_size(&info.ty));
    store_byte_aggregate_field(b, desc, 8, IrType::Int(IntWidth::I64), elem_size);
    let rank = b.const_i32(info.dims.len() as i32);
    store_byte_aggregate_field(b, desc, 16, IrType::Int(IntWidth::I32), rank);
    let flags = b.const_i32(2);
    store_byte_aggregate_field(b, desc, 20, IrType::Int(IntWidth::I32), flags);

    for (i, (lower, extent)) in info.dims.iter().copied().enumerate() {
        let base_offset = 24 + (i as i64) * 24;
        let lower_val = b.const_i64(lower);
        store_byte_aggregate_field(b, desc, base_offset, IrType::Int(IntWidth::I64), lower_val);
        let upper_val = b.const_i64(lower + extent - 1);
        store_byte_aggregate_field(
            b,
            desc,
            base_offset + 8,
            IrType::Int(IntWidth::I64),
            upper_val,
        );
        let stride_val = b.const_i64(1);
        store_byte_aggregate_field(b, desc, base_offset + 16, IrType::Int(IntWidth::I64), stride_val);
    }

    desc
}

fn lower_arg_descriptor(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    _st: &SymbolTable,
) -> ValueId {
    if let Expr::Name { name } = &expr.node {
        let key = name.to_lowercase();
        if let Some(info) = locals.get(&key) {
            if info.allocatable {
                return info.addr;
            }
            if info.descriptor_arg {
                return b.load(info.addr);
            }
            if !info.dims.is_empty() {
                return materialize_array_descriptor_for_info(b, info);
            }
        }
    }
    b.const_i64(0)
}

/// Get the data base address for an array variable.
/// For fixed arrays, this is the alloca address directly.
/// For allocatable arrays, load base_addr from the descriptor (offset 0).
fn array_base_addr(b: &mut FuncBuilder, info: &LocalInfo) -> ValueId {
    if local_uses_array_descriptor(info) {
        let desc = array_descriptor_addr(b, info);
        b.load_typed(desc, IrType::Ptr(Box::new(info.ty.clone())))
    } else if info.by_ref {
        // Dummy arrays are stored as "slot holding caller base pointer".
        b.load(info.addr)
    } else {
        info.addr
    }
}

fn array_total_elems_value(b: &mut FuncBuilder, info: &LocalInfo) -> ValueId {
    if local_uses_array_descriptor(info) {
        let desc = array_descriptor_addr(b, info);
        b.call(
            FuncRef::External("afs_array_size".into()),
            vec![desc],
            IrType::Int(IntWidth::I64),
        )
    } else {
        let total: i64 = info.dims.iter().map(|(_, extent)| *extent).product();
        b.const_i64(total.max(0))
    }
}

fn whole_array_expr_info(
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
) -> Option<LocalInfo> {
    let Expr::Name { name } = &expr.node else {
        return None;
    };
    let key = name.to_lowercase();
    locals.get(&key)
        .filter(|info| !info.dims.is_empty() || info.allocatable)
        .cloned()
}

fn whole_array_named_info(
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
) -> Option<(String, LocalInfo)> {
    let Expr::Name { name } = &expr.node else {
        return None;
    };
    let key = name.to_lowercase();
    let info = locals
        .get(&key)
        .filter(|info| !info.dims.is_empty() || info.allocatable)
        .cloned()?;
    Some((key, info))
}

#[derive(Clone)]
enum BulkArrayPlan {
    Fill {
        kernel: &'static str,
        scalar: crate::ast::expr::SpannedExpr,
    },
    ArrayBinary {
        kernel: &'static str,
        lhs: LocalInfo,
        rhs: LocalInfo,
    },
    ArrayScalar {
        kernel: &'static str,
        array: LocalInfo,
        scalar: crate::ast::expr::SpannedExpr,
    },
    ScalarArray {
        kernel: &'static str,
        scalar: crate::ast::expr::SpannedExpr,
        array: LocalInfo,
    },
}

#[derive(Clone)]
struct IndexedArrayRef {
    name: String,
    info: LocalInfo,
}

fn bulk_fill_runtime_name(ty: &IrType) -> Option<&'static str> {
    match ty {
        IrType::Int(IntWidth::I32) => Some("afs_fill_i32"),
        IrType::Float(FloatWidth::F32) => Some("afs_fill_f32"),
        IrType::Float(FloatWidth::F64) => Some("afs_fill_f64"),
        _ => None,
    }
}

fn bulk_array_binary_runtime_name(op: BinaryOp, ty: &IrType) -> Option<&'static str> {
    match (op, ty) {
        (BinaryOp::Add, IrType::Int(IntWidth::I32)) => Some("afs_array_add_i32"),
        (BinaryOp::Add, IrType::Float(FloatWidth::F32)) => Some("afs_array_add_f32"),
        (BinaryOp::Add, IrType::Float(FloatWidth::F64)) => Some("afs_array_add_f64"),
        (BinaryOp::Sub, IrType::Int(IntWidth::I32)) => Some("afs_array_sub_i32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_array_sub_f32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_array_sub_f64"),
        (BinaryOp::Mul, IrType::Int(IntWidth::I32)) => Some("afs_array_mul_i32"),
        (BinaryOp::Mul, IrType::Float(FloatWidth::F32)) => Some("afs_array_mul_f32"),
        (BinaryOp::Mul, IrType::Float(FloatWidth::F64)) => Some("afs_array_mul_f64"),
        _ => None,
    }
}

fn bulk_array_scalar_runtime_name(op: BinaryOp, ty: &IrType) -> Option<&'static str> {
    match (op, ty) {
        (BinaryOp::Add, IrType::Int(IntWidth::I32)) => Some("afs_array_add_scalar_i32"),
        (BinaryOp::Add, IrType::Float(FloatWidth::F32)) => Some("afs_array_add_scalar_f32"),
        (BinaryOp::Add, IrType::Float(FloatWidth::F64)) => Some("afs_array_add_scalar_f64"),
        (BinaryOp::Sub, IrType::Int(IntWidth::I32)) => Some("afs_array_sub_scalar_i32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_array_sub_scalar_f32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_array_sub_scalar_f64"),
        (BinaryOp::Mul, IrType::Int(IntWidth::I32)) => Some("afs_array_mul_scalar_i32"),
        (BinaryOp::Mul, IrType::Float(FloatWidth::F32)) => Some("afs_array_mul_scalar_f32"),
        (BinaryOp::Mul, IrType::Float(FloatWidth::F64)) => Some("afs_array_mul_scalar_f64"),
        _ => None,
    }
}

fn bulk_scalar_array_runtime_name(op: BinaryOp, ty: &IrType) -> Option<&'static str> {
    match (op, ty) {
        (BinaryOp::Sub, IrType::Int(IntWidth::I32)) => Some("afs_scalar_sub_array_i32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_scalar_sub_array_f32"),
        (BinaryOp::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_scalar_sub_array_f64"),
        _ => None,
    }
}

fn expr_contains_array_refs(
    expr: &crate::ast::expr::SpannedExpr,
    locals: &HashMap<String, LocalInfo>,
) -> bool {
    let mut arrays = Vec::new();
    collect_array_names(expr, locals, &mut arrays);
    !arrays.is_empty()
}

fn expr_mentions_name(expr: &crate::ast::expr::SpannedExpr, needle: &str) -> bool {
    match &expr.node {
        Expr::Name { name } => name.eq_ignore_ascii_case(needle),
        Expr::BinaryOp { left, right, .. } => {
            expr_mentions_name(left, needle) || expr_mentions_name(right, needle)
        }
        Expr::UnaryOp { operand, .. } => expr_mentions_name(operand, needle),
        Expr::ParenExpr { inner } => expr_mentions_name(inner, needle),
        Expr::ComponentAccess { base, .. } => expr_mentions_name(base, needle),
        Expr::FunctionCall { callee, args } => {
            expr_mentions_name(callee, needle)
                || args.iter().any(|arg| match &arg.value {
                    crate::ast::expr::SectionSubscript::Element(e) => expr_mentions_name(e, needle),
                    crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
                        start.as_ref().is_some_and(|e| expr_mentions_name(e, needle))
                            || end.as_ref().is_some_and(|e| expr_mentions_name(e, needle))
                            || stride.as_ref().is_some_and(|e| expr_mentions_name(e, needle))
                    }
                })
        }
        Expr::ArrayConstructor { values, .. } => values.iter().any(|v| match v {
            crate::ast::expr::AcValue::Expr(e) => expr_mentions_name(e, needle),
            crate::ast::expr::AcValue::ImpliedDo(ido) => {
                ido.var.eq_ignore_ascii_case(needle)
                    || expr_mentions_name(&ido.start, needle)
                    || expr_mentions_name(&ido.end, needle)
                    || ido.step.as_ref().is_some_and(|e| expr_mentions_name(e, needle))
                    || ido.values.iter().any(|inner| match inner {
                        crate::ast::expr::AcValue::Expr(e) => expr_mentions_name(e, needle),
                        crate::ast::expr::AcValue::ImpliedDo(_) => false,
                    })
            }
        }),
        _ => false,
    }
}

fn expr_is_size_of_array(expr: &crate::ast::expr::SpannedExpr, array_name: &str) -> bool {
    match &expr.node {
        Expr::ParenExpr { inner } => expr_is_size_of_array(inner, array_name),
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                if name.eq_ignore_ascii_case("size") && args.len() == 1 {
                    if let crate::ast::expr::SectionSubscript::Element(arg) = &args[0].value {
                        return matches!(
                            &arg.node,
                            Expr::Name { name } if name.eq_ignore_ascii_case(array_name)
                        );
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn fresh_synth_loop_var(locals: &HashMap<String, LocalInfo>) -> String {
    let mut idx = 0usize;
    loop {
        let name = if idx == 0 {
            "afs_elem_i".to_string()
        } else {
            format!("afs_elem_i{}", idx)
        };
        if !locals.contains_key(&name) {
            return name;
        }
        idx += 1;
    }
}

fn synth_name_expr(name: &str, span: crate::lexer::Span) -> crate::ast::expr::SpannedExpr {
    crate::ast::Spanned::new(Expr::Name { name: name.to_string() }, span)
}

fn synth_int_expr(value: i64, span: crate::lexer::Span) -> crate::ast::expr::SpannedExpr {
    crate::ast::Spanned::new(
        Expr::IntegerLiteral {
            text: value.to_string(),
            kind: None,
        },
        span,
    )
}

fn synth_indexed_array_expr(
    array_name: &str,
    index_name: &str,
    span: crate::lexer::Span,
) -> crate::ast::expr::SpannedExpr {
    crate::ast::Spanned::new(
        Expr::FunctionCall {
            callee: Box::new(synth_name_expr(array_name, span)),
            args: vec![crate::ast::expr::Argument {
                keyword: None,
                value: crate::ast::expr::SectionSubscript::Element(synth_name_expr(index_name, span)),
            }],
        },
        span,
    )
}

fn try_lower_elemental_array_assign(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    dest_name: &str,
    dest_info: &LocalInfo,
    value: &crate::ast::expr::SpannedExpr,
) -> bool {
    if dest_info.allocatable || dest_info.dims.len() != 1 {
        return false;
    }

    let Expr::FunctionCall { callee, args } = &value.node else {
        return false;
    };
    let Expr::Name { name: callee_name } = &callee.node else {
        return false;
    };
    if !ctx.elemental_funcs.contains(&callee_name.to_lowercase()) {
        return false;
    }

    let (dest_lower, dest_extent) = dest_info.dims[0];
    let dest_upper = dest_lower + dest_extent - 1;
    let loop_var = fresh_synth_loop_var(&ctx.locals);
    let mut saw_array_arg = false;
    let mut mapped_args = Vec::with_capacity(args.len());

    for arg in args {
        if arg.keyword.is_some() {
            return false;
        }
        let crate::ast::expr::SectionSubscript::Element(actual) = &arg.value else {
            return false;
        };

        if let Some((array_name, array_info)) = whole_array_named_info(&ctx.locals, actual) {
            if array_info.allocatable
                || array_info.dims.len() != 1
                || !bulk_arrays_compatible(dest_info, &array_info)
            {
                return false;
            }
            saw_array_arg = true;
            mapped_args.push(crate::ast::expr::Argument {
                keyword: None,
                value: crate::ast::expr::SectionSubscript::Element(
                    synth_indexed_array_expr(&array_name, &loop_var, actual.span),
                ),
            });
        } else {
            if expr_contains_array_refs(actual, &ctx.locals) {
                return false;
            }
            mapped_args.push(arg.clone());
        }
    }

    if !saw_array_arg {
        return false;
    }

    let target = synth_indexed_array_expr(dest_name, &loop_var, value.span);
    let mapped_value = crate::ast::Spanned::new(
        Expr::FunctionCall {
            callee: Box::new(synth_name_expr(callee_name, callee.span)),
            args: mapped_args,
        },
        value.span,
    );
    let body = vec![crate::ast::Spanned::new(
        Stmt::Assignment {
            target,
            value: mapped_value,
        },
        value.span,
    )];
    let controls = vec![ConcurrentControl {
        var: loop_var,
        start: synth_int_expr(dest_lower, value.span),
        end: synth_int_expr(dest_upper, value.span),
        step: None,
    }];
    lower_do_concurrent(b, ctx, &None, &controls, None, &body, value.span);
    true
}

fn bulk_arrays_compatible(dest_info: &LocalInfo, other_info: &LocalInfo) -> bool {
    if dest_info.ty != other_info.ty {
        return false;
    }
    if dest_info.allocatable || other_info.allocatable {
        return true;
    }
    dest_info.dims == other_info.dims
}

fn build_whole_array_bulk_plan(
    locals: &HashMap<String, LocalInfo>,
    dest_info: &LocalInfo,
    value: &crate::ast::expr::SpannedExpr,
) -> Option<BulkArrayPlan> {
    if let Expr::BinaryOp { op, left, right } = &value.node {
        if let Some(kernel) = bulk_array_binary_runtime_name(op.clone(), &dest_info.ty) {
            let lhs_info = whole_array_expr_info(locals, left);
            let rhs_info = whole_array_expr_info(locals, right);
            if let (Some(lhs_info), Some(rhs_info)) = (lhs_info, rhs_info) {
                if bulk_arrays_compatible(dest_info, &lhs_info)
                    && bulk_arrays_compatible(dest_info, &rhs_info)
                {
                    return Some(BulkArrayPlan::ArrayBinary { kernel, lhs: lhs_info, rhs: rhs_info });
                }
            }
        }

        let lhs_info = whole_array_expr_info(locals, left);
        let rhs_info = whole_array_expr_info(locals, right);
        let lhs_scalar = !expr_contains_array_refs(left, locals);
        let rhs_scalar = !expr_contains_array_refs(right, locals);

        if let Some(lhs_info) = lhs_info {
            if rhs_scalar && bulk_arrays_compatible(dest_info, &lhs_info) {
                if let Some(kernel) = bulk_array_scalar_runtime_name(op.clone(), &dest_info.ty) {
                    return Some(BulkArrayPlan::ArrayScalar {
                        kernel,
                        array: lhs_info,
                        scalar: (**right).clone(),
                    });
                }
            }
        }

        if let Some(rhs_info) = rhs_info {
            if lhs_scalar && bulk_arrays_compatible(dest_info, &rhs_info) {
                match op {
                    BinaryOp::Add | BinaryOp::Mul => {
                        if let Some(kernel) = bulk_array_scalar_runtime_name(op.clone(), &dest_info.ty) {
                            return Some(BulkArrayPlan::ArrayScalar {
                                kernel,
                                array: rhs_info,
                                scalar: (**left).clone(),
                            });
                        }
                    }
                    BinaryOp::Sub => {
                        if let Some(kernel) = bulk_scalar_array_runtime_name(op.clone(), &dest_info.ty) {
                            return Some(BulkArrayPlan::ScalarArray {
                                kernel,
                                scalar: (**left).clone(),
                                array: rhs_info,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if !expr_contains_array_refs(value, locals) {
        if let Some(kernel) = bulk_fill_runtime_name(&dest_info.ty) {
            return Some(BulkArrayPlan::Fill {
                kernel,
                scalar: value.clone(),
            });
        }
    }

    None
}

fn emit_bulk_array_plan(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    dest_info: &LocalInfo,
    n: ValueId,
    plan: BulkArrayPlan,
) {
    let dest_base = array_base_addr(b, dest_info);
    match plan {
        BulkArrayPlan::Fill { kernel, scalar } => {
            let scalar = lower_expr_tl(b, &ctx.locals, &scalar, ctx.st, ctx.type_layouts);
            b.call(FuncRef::External(kernel.into()), vec![dest_base, n, scalar], IrType::Void);
        }
        BulkArrayPlan::ArrayBinary { kernel, lhs, rhs } => {
            let lhs_base = array_base_addr(b, &lhs);
            let rhs_base = array_base_addr(b, &rhs);
            b.call(
                FuncRef::External(kernel.into()),
                vec![dest_base, lhs_base, rhs_base, n],
                IrType::Void,
            );
        }
        BulkArrayPlan::ArrayScalar { kernel, array, scalar } => {
            let array_base = array_base_addr(b, &array);
            let scalar = lower_expr_tl(b, &ctx.locals, &scalar, ctx.st, ctx.type_layouts);
            b.call(
                FuncRef::External(kernel.into()),
                vec![dest_base, array_base, scalar, n],
                IrType::Void,
            );
        }
        BulkArrayPlan::ScalarArray { kernel, scalar, array } => {
            let scalar = lower_expr_tl(b, &ctx.locals, &scalar, ctx.st, ctx.type_layouts);
            let array_base = array_base_addr(b, &array);
            b.call(
                FuncRef::External(kernel.into()),
                vec![dest_base, scalar, array_base, n],
                IrType::Void,
            );
        }
    }
}

fn loop_indexed_array_ref(
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    loop_var: &str,
) -> Option<IndexedArrayRef> {
    match &expr.node {
        Expr::ParenExpr { inner } => loop_indexed_array_ref(locals, inner, loop_var),
        Expr::FunctionCall { callee, args } => {
            if args.len() != 1 {
                return None;
            }
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let crate::ast::expr::SectionSubscript::Element(index) = &args[0].value else {
                return None;
            };
            let Expr::Name { name: idx_name } = &index.node else {
                return None;
            };
            if !idx_name.eq_ignore_ascii_case(loop_var) {
                return None;
            }
            let key = name.to_lowercase();
            let info = locals.get(&key)?.clone();
            if info.allocatable || info.dims.len() == 1 {
                Some(IndexedArrayRef { name: key, info })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn control_covers_full_array(ctrl: &ConcurrentControl, dest: &IndexedArrayRef) -> bool {
    let step_ok = ctrl.step.as_ref().is_none_or(|step| eval_const_int(step) == Some(1));
    if !step_ok {
        return false;
    }
    if dest.info.allocatable {
        eval_const_int(&ctrl.start) == Some(1) && expr_is_size_of_array(&ctrl.end, &dest.name)
    } else {
        let Some((lower, extent)) = dest.info.dims.first().copied() else {
            return false;
        };
        let upper = lower + extent - 1;
        eval_const_int(&ctrl.start) == Some(lower) && eval_const_int(&ctrl.end) == Some(upper)
    }
}

fn build_loop_bulk_plan(
    locals: &HashMap<String, LocalInfo>,
    dest_info: &LocalInfo,
    loop_var: &str,
    value: &crate::ast::expr::SpannedExpr,
) -> Option<BulkArrayPlan> {
    if let Expr::BinaryOp { op, left, right } = &value.node {
        if let Some(kernel) = bulk_array_binary_runtime_name(op.clone(), &dest_info.ty) {
            let lhs = loop_indexed_array_ref(locals, left, loop_var);
            let rhs = loop_indexed_array_ref(locals, right, loop_var);
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if bulk_arrays_compatible(dest_info, &lhs.info)
                    && bulk_arrays_compatible(dest_info, &rhs.info)
                {
                    return Some(BulkArrayPlan::ArrayBinary {
                        kernel,
                        lhs: lhs.info,
                        rhs: rhs.info,
                    });
                }
            }
        }

        let lhs = loop_indexed_array_ref(locals, left, loop_var);
        let rhs = loop_indexed_array_ref(locals, right, loop_var);
        let lhs_scalar = !expr_contains_array_refs(left, locals) && !expr_mentions_name(left, loop_var);
        let rhs_scalar = !expr_contains_array_refs(right, locals) && !expr_mentions_name(right, loop_var);

        if let Some(lhs) = lhs {
            if rhs_scalar && bulk_arrays_compatible(dest_info, &lhs.info) {
                if let Some(kernel) = bulk_array_scalar_runtime_name(op.clone(), &dest_info.ty) {
                    return Some(BulkArrayPlan::ArrayScalar {
                        kernel,
                        array: lhs.info,
                        scalar: (**right).clone(),
                    });
                }
            }
        }

        if let Some(rhs) = rhs {
            if lhs_scalar && bulk_arrays_compatible(dest_info, &rhs.info) {
                match op {
                    BinaryOp::Add | BinaryOp::Mul => {
                        if let Some(kernel) = bulk_array_scalar_runtime_name(op.clone(), &dest_info.ty) {
                            return Some(BulkArrayPlan::ArrayScalar {
                                kernel,
                                array: rhs.info,
                                scalar: (**left).clone(),
                            });
                        }
                    }
                    BinaryOp::Sub => {
                        if let Some(kernel) = bulk_scalar_array_runtime_name(op.clone(), &dest_info.ty) {
                            return Some(BulkArrayPlan::ScalarArray {
                                kernel,
                                scalar: (**left).clone(),
                                array: rhs.info,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if !expr_contains_array_refs(value, locals) && !expr_mentions_name(value, loop_var) {
        if let Some(kernel) = bulk_fill_runtime_name(&dest_info.ty) {
            return Some(BulkArrayPlan::Fill {
                kernel,
                scalar: value.clone(),
            });
        }
    }

    None
}

fn try_lower_bulk_array_assign(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    dest_info: &LocalInfo,
    value: &crate::ast::expr::SpannedExpr,
) -> bool {
    if let Some(plan) = build_whole_array_bulk_plan(&ctx.locals, dest_info, value) {
        let n = array_total_elems_value(b, dest_info);
        emit_bulk_array_plan(b, ctx, dest_info, n, plan);
        return true;
    }
    false
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
/// Lower FORALL by nesting loops recursively. The body executes inside the innermost loop.
fn lower_forall_nested(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    specs: &[crate::ast::stmt::ForallSpec],
    mask: Option<&crate::ast::expr::SpannedExpr>,
    body: &[SpannedStmt],
) {
    if specs.is_empty() {
        // Innermost level: apply mask and execute body.
        if let Some(mask_expr) = mask {
            let cond = lower_expr_tl(b, &ctx.locals, mask_expr, ctx.st, ctx.type_layouts);
            let bb_body = b.create_block("forall_body");
            let bb_skip = b.create_block("forall_skip");
            b.cond_branch(cond, bb_body, vec![], bb_skip, vec![]);
            b.set_block(bb_body);
            lower_stmts(b, ctx, body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_skip, vec![]);
            }
            b.set_block(bb_skip);
        } else {
            lower_stmts(b, ctx, body);
        }
    } else {
        // Wrap remaining specs in a DO loop. The loop body recurses to handle inner specs.
        let spec = &specs[0];
        let remaining = &specs[1..];

        // Build the inner body as a FORALL of remaining specs + body.
        // We use lower_do_loop with the body being the recursive FORALL.
        // But lower_do_loop takes &[SpannedStmt], not a closure.
        // Instead, manually build the loop structure.
        let key = spec.var.to_lowercase();
        let var_addr = ctx.locals.get(&key).map(|info| info.addr).unwrap_or_else(|| {
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            ctx.locals.insert(key.clone(), LocalInfo {
                addr, ty: IrType::Int(IntWidth::I32), dims: vec![],
                allocatable: false, descriptor_arg: false, by_ref: false, char_kind: CharKind::None,
                derived_type: None, inline_const: None, is_pointer: false,
            });
            addr
        });

        let init_val = lower_expr(b, &ctx.locals, &spec.start, ctx.st);
        b.store(init_val, var_addr);
        let end_val = lower_expr(b, &ctx.locals, &spec.end, ctx.st);
        let step_val = spec.step.as_ref()
            .map(|s| lower_expr(b, &ctx.locals, s, ctx.st))
            .unwrap_or_else(|| b.const_i32(1));

        let bb_check = b.create_block("forall_check");
        let bb_loop = b.create_block("forall_loop");
        let bb_incr = b.create_block("forall_incr");
        let bb_exit = b.create_block("forall_exit");
        b.branch(bb_check, vec![]);

        b.set_block(bb_check);
        let cur = b.load(var_addr);
        // Handle both positive and negative steps: done = (step >= 0 && cur > end) || (step < 0 && cur < end)
        let zero_const = b.const_i32(0);
        let step_neg = b.icmp(CmpOp::Lt, step_val, zero_const);
        let gt_end = b.icmp(CmpOp::Gt, cur, end_val);
        let lt_end = b.icmp(CmpOp::Lt, cur, end_val);
        let done = b.select(step_neg, lt_end, gt_end);
        b.cond_branch(done, bb_exit, vec![], bb_loop, vec![]);

        b.set_block(bb_loop);
        // Recurse: lower remaining specs + body inside this loop.
        lower_forall_nested(b, ctx, remaining, mask, body);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_incr, vec![]);
        }

        b.set_block(bb_incr);
        let cur2 = b.load(var_addr);
        let next = b.iadd(cur2, step_val);
        b.store(next, var_addr);
        b.branch(bb_check, vec![]);

        b.set_block(bb_exit);
    }
}

/// Lower whole-array assignment: a = b (element-wise copy) or a = scalar (broadcast).
fn lower_array_assign(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    dest_name: &str,
    dest_info: &LocalInfo,
    value: &crate::ast::expr::SpannedExpr,
) {
    // a = [v0, v1, v2, ...] — element-wise store of an array
    // constructor's literal values into the destination.
    if let Expr::ArrayConstructor { values, .. } = &value.node {
        let dest_base = array_base_addr(b, dest_info);
        store_ac_values_into(b, &ctx.locals, dest_base, &dest_info.ty, values, ctx.st);
        return;
    }

    if try_lower_elemental_array_assign(b, ctx, dest_name, dest_info, value) {
        return;
    }

    if try_lower_bulk_array_assign(b, ctx, dest_info, value) {
        return;
    }

    // Check if RHS is also an array variable → element-wise copy via memcpy.
    let rhs_is_array = if let Expr::Name { name } = &value.node {
        ctx.locals.get(&name.to_lowercase())
            .map(|i| !i.dims.is_empty() || i.allocatable)
            .unwrap_or(false)
    } else { false };

    if rhs_is_array {
        // a = b: memcpy from b's data to a's data.
        let dest_base = array_base_addr(b, dest_info);

        if let Expr::Name { name } = &value.node {
            let key = name.to_lowercase();
            if let Some(src_info) = ctx.locals.get(&key) {
                let src_base = array_base_addr(b, src_info);

                // Compute byte count: size(a) * elem_size.
                let n = array_total_elems_value(b, dest_info);
                let elem_bytes = b.const_i64(ir_scalar_byte_size(&dest_info.ty));
                let byte_count = b.imul(n, elem_bytes);
                b.call(FuncRef::External("memcpy".into()),
                    vec![dest_base, src_base, byte_count],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            }
        }
    } else {
        // a = scalar: broadcast scalar to all elements.
        // Generate a loop with stack-allocated counter.
        let scalar = lower_expr_tl(b, &ctx.locals, value, ctx.st, ctx.type_layouts);
        let dest_base = array_base_addr(b, dest_info);
        let n = array_total_elems_value(b, dest_info);

        // Stack-allocated loop counter.
        let i_addr = b.alloca(IrType::Int(IntWidth::I64));
        let zero = b.const_i64(0);
        b.store(zero, i_addr);

        let bb_check = b.create_block("broadcast_check");
        let bb_body = b.create_block("broadcast_body");
        let bb_exit = b.create_block("broadcast_exit");
        b.branch(bb_check, vec![]);

        b.set_block(bb_check);
        let i = b.load(i_addr);
        let done = b.icmp(CmpOp::Ge, i, n);
        b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

        b.set_block(bb_body);
        let i_val = b.load(i_addr);
        // Compute byte offset: i * elem_size. Use byte-level GEP to avoid double multiplication.
        let elem_bytes = match &dest_info.ty {
            IrType::Int(IntWidth::I128) => b.const_i64(16),
            IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => b.const_i64(8),
            IrType::Int(IntWidth::I16) => b.const_i64(2),
            IrType::Int(IntWidth::I8) => b.const_i64(1),
            _ => b.const_i64(4),
        };
        let byte_offset = b.imul(i_val, elem_bytes);
        let elem_ptr = b.gep(dest_base, vec![byte_offset], IrType::Int(IntWidth::I8));
        b.store(scalar, elem_ptr);
        let one = b.const_i64(1);
        let next_i = b.iadd(i_val, one);
        b.store(next_i, i_addr);
        b.branch(bb_check, vec![]);

        b.set_block(bb_exit);
    }
}

/// Collect all array variable names referenced in an expression.
fn collect_array_names(expr: &crate::ast::expr::SpannedExpr, locals: &HashMap<String, LocalInfo>, out: &mut Vec<String>) {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            if let Some(info) = locals.get(&key) {
                if (!info.dims.is_empty() || info.allocatable) && !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_array_names(left, locals, out);
            collect_array_names(right, locals, out);
        }
        Expr::UnaryOp { operand, .. } => collect_array_names(operand, locals, out),
        Expr::ParenExpr { inner } => collect_array_names(inner, locals, out),
        Expr::FunctionCall { args, .. } => {
            if let Expr::FunctionCall { callee, .. } = &expr.node {
                collect_array_names(callee, locals, out);
            }
            for a in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    collect_array_names(e, locals, out);
                } else if let crate::ast::expr::SectionSubscript::Range { start, end, stride } = &a.value {
                    if let Some(e) = start {
                        collect_array_names(e, locals, out);
                    }
                    if let Some(e) = end {
                        collect_array_names(e, locals, out);
                    }
                    if let Some(e) = stride {
                        collect_array_names(e, locals, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Collect array names referenced in a statement (for WHERE body analysis).
fn collect_array_names_stmt(stmt: &SpannedStmt, locals: &HashMap<String, LocalInfo>, out: &mut Vec<String>) {
    if let Stmt::Assignment { target, value } = &stmt.node {
        collect_array_names(target, locals, out);
        collect_array_names(value, locals, out);
    }
}

/// Find the first array variable referenced in an expression (for WHERE mask detection).
fn find_array_in_expr(expr: &crate::ast::expr::SpannedExpr, locals: &HashMap<String, LocalInfo>) -> Option<LocalInfo> {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            locals.get(&key).filter(|i| !i.dims.is_empty() || i.allocatable).cloned()
        }
        Expr::BinaryOp { left, right, .. } => {
            find_array_in_expr(left, locals).or_else(|| find_array_in_expr(right, locals))
        }
        Expr::UnaryOp { operand, .. } => find_array_in_expr(operand, locals),
        Expr::ParenExpr { inner } => find_array_in_expr(inner, locals),
        Expr::FunctionCall { callee, args } => {
            find_array_in_expr(callee, locals).or_else(|| args.iter().find_map(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    find_array_in_expr(e, locals)
                } else if let crate::ast::expr::SectionSubscript::Range { start, end, stride } = &a.value {
                    start.as_ref()
                        .and_then(|e| find_array_in_expr(e, locals))
                        .or_else(|| end.as_ref().and_then(|e| find_array_in_expr(e, locals)))
                        .or_else(|| stride.as_ref().and_then(|e| find_array_in_expr(e, locals)))
                } else { None }
            }))
        }
        _ => None,
    }
}

/// Check if the first argument refers to a REAL array (for type dispatch).
fn first_arg_is_real(args: &[crate::ast::expr::Argument], locals: &HashMap<String, LocalInfo>) -> bool {
    args.first().and_then(|a| {
        if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
            if let Expr::Name { name } = &e.node {
                locals.get(&name.to_lowercase()).map(|i| i.ty.is_float())
            } else { None }
        } else { None }
    }).unwrap_or(false)
}

/// Lower an array section expression: a(1:10:2) → create section descriptor.
fn lower_array_section(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    st: &SymbolTable,
) -> ValueId {
    let n_dims = args.len();

    // Allocate SectionSpec array on stack: each spec is 24 bytes (3 x i64).
    let spec_array_size = (n_dims * 24) as u64;
    let specs = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), spec_array_size));

    // Fill in each SectionSpec from the subscript ranges.
    for (i, arg) in args.iter().enumerate() {
        let base_offset = (i * 24) as i64;
        match &arg.value {
            crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
                let start_val = start.as_ref()
                    .map(|e| {
                        let raw = lower_expr(b, locals, e, st);
                        widen_idx_to_i64(b, raw)
                    })
                    .unwrap_or_else(|| {
                        if local_uses_array_descriptor(info) {
                            let dim = b.const_i32((i + 1) as i32);
                            let desc = array_descriptor_addr(b, info);
                            b.call(
                                FuncRef::External("afs_array_lbound".into()),
                                vec![desc, dim],
                                IrType::Int(IntWidth::I64),
                            )
                        } else {
                            let lower = info.dims.get(i).copied().map(|(lo, _)| lo).unwrap_or(1);
                            b.const_i64(lower)
                        }
                    });
                let end_val = end.as_ref()
                    .map(|e| {
                        let raw = lower_expr(b, locals, e, st);
                        widen_idx_to_i64(b, raw)
                    })
                    .unwrap_or_else(|| {
                        if local_uses_array_descriptor(info) {
                            let dim = b.const_i32((i + 1) as i32);
                            let desc = array_descriptor_addr(b, info);
                            b.call(
                                FuncRef::External("afs_array_ubound".into()),
                                vec![desc, dim],
                                IrType::Int(IntWidth::I64),
                            )
                        } else {
                            let (lower, extent) = info.dims.get(i).copied().unwrap_or((1, 1));
                            b.const_i64(lower + extent - 1)
                        }
                    });
                let stride_val = stride.as_ref()
                    .map(|e| {
                        let raw = lower_expr(b, locals, e, st);
                        widen_idx_to_i64(b, raw)
                    })
                    .unwrap_or_else(|| b.const_i64(1)); // default stride = 1

                // Store start at offset+0, end at offset+8, stride at offset+16.
                let off0 = b.const_i64(base_offset);
                let off8 = b.const_i64(base_offset + 8);
                let off16 = b.const_i64(base_offset + 16);
                let p0 = b.gep(specs, vec![off0], IrType::Int(IntWidth::I8));
                let p8 = b.gep(specs, vec![off8], IrType::Int(IntWidth::I8));
                let p16 = b.gep(specs, vec![off16], IrType::Int(IntWidth::I8));
                b.store(start_val, p0);
                b.store(end_val, p8);
                b.store(stride_val, p16);
            }
            crate::ast::expr::SectionSubscript::Element(e) => {
                // Single element subscript in a section context — treat as start=end=val, stride=1.
                let raw = lower_expr(b, locals, e, st);
                let val = widen_idx_to_i64(b, raw);
                let off0 = b.const_i64(base_offset);
                let off8 = b.const_i64(base_offset + 8);
                let off16 = b.const_i64(base_offset + 16);
                let p0 = b.gep(specs, vec![off0], IrType::Int(IntWidth::I8));
                let p8 = b.gep(specs, vec![off8], IrType::Int(IntWidth::I8));
                let p16 = b.gep(specs, vec![off16], IrType::Int(IntWidth::I8));
                b.store(val, p0);
                b.store(val, p8);
                let one = b.const_i64(1);
                b.store(one, p16);
            }
        }
    }

    // Allocate result descriptor on stack (384 bytes).
    let result_desc = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384));
    let zero = b.const_i32(0);
    let sz384 = b.const_i64(384);
    b.call(FuncRef::External("memset".into()), vec![result_desc, zero, sz384],
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));

    // Call afs_create_section(source, result, specs, n_dims).
    let ndims = b.const_i32(n_dims as i32);
    let source_desc = if local_uses_array_descriptor(info) {
        array_descriptor_addr(b, info)
    } else {
        materialize_array_descriptor_for_info(b, info)
    };
    b.call(FuncRef::External("afs_create_section".into()),
        vec![source_desc, result_desc, specs, ndims], IrType::Void);

    result_desc
}

/// Lower array intrinsics that need descriptor addresses (SIZE, SUM, etc.).
/// Lower pointer-only intrinsics like `ASSOCIATED(p)`.  Kept
/// separate from `lower_array_intrinsic` because the argument
/// filter there rejects scalar and derived-type pointers (they
/// don't have array dims) — but ASSOCIATED works on every
/// pointer shape.
///
/// Returns `Some(bool_value)` for `ASSOCIATED(p)`, `None` for
/// any other name or shape so the caller can fall through.
fn lower_pointer_intrinsic(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    name: &str,
    args: &[crate::ast::expr::Argument],
) -> Option<ValueId> {
    if name != "associated" {
        return None;
    }
    // We handle the one-argument form: ASSOCIATED(p).  The
    // two-argument form ASSOCIATED(p, target) is deferred.
    let first = args.first()?;
    let crate::ast::expr::SectionSubscript::Element(expr) = &first.value else { return None; };
    let Expr::Name { name: ptr_name } = &expr.node else { return None; };
    let info = locals.get(&ptr_name.to_lowercase())?.clone();
    if !info.is_pointer {
        return None;
    }
    // Load the pointer's stored target address (byte offset 0).
    let zero_off = b.const_i64(0);
    let base_ptr = b.gep(info.addr, vec![zero_off], IrType::Int(IntWidth::I64));
    let raw = b.load_typed(base_ptr, IrType::Int(IntWidth::I64));
    let zero = b.const_i64(0);

    if args.len() >= 2 {
        // Two-argument form: ASSOCIATED(p, target).
        // True iff p's stored address equals the target's address.
        // Both values are compared as raw i64 representations.
        let second = &args[1];
        let crate::ast::expr::SectionSubscript::Element(tgt_expr) = &second.value else {
            return Some(b.icmp(CmpOp::Ne, raw, zero));
        };
        let Expr::Name { name: tgt_name } = &tgt_expr.node else {
            return Some(b.icmp(CmpOp::Ne, raw, zero));
        };
        let Some(tgt_info) = locals.get(&tgt_name.to_lowercase()) else {
            return Some(b.icmp(CmpOp::Ne, raw, zero));
        };
        // Get the target's address as i64 for comparison.
        // For a pointer: load the stored address from its slot.
        // For a plain variable: write info.addr into a scratch
        // i64 slot and read it back (effectlvely ptrtoint).
        let tgt_addr = if tgt_info.is_pointer {
            let off = b.const_i64(0);
            let tgt_slot = b.gep(tgt_info.addr, vec![off], IrType::Int(IntWidth::I64));
            b.load_typed(tgt_slot, IrType::Int(IntWidth::I64))
        } else {
            let scratch = b.alloca(IrType::Ptr(Box::new(info.ty.clone())));
            b.store(tgt_info.addr, scratch);
            b.load_typed(scratch, IrType::Int(IntWidth::I64))
        };
        return Some(b.icmp(CmpOp::Eq, raw, tgt_addr));
    }

    Some(b.icmp(CmpOp::Ne, raw, zero))
}

fn lower_array_intrinsic(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    name: &str,
    args: &[crate::ast::expr::Argument],
    st: &SymbolTable,
) -> Option<ValueId> {
    let first_arg_info = args.first().and_then(|a| {
        if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
            if let Expr::Name { name } = &e.node {
                let key = name.to_lowercase();
                locals.get(&key).cloned().filter(|i| local_uses_array_descriptor(i) || !i.dims.is_empty())
            } else { None }
        } else { None }
    });

    let info = first_arg_info?;
    let desc = if local_uses_array_descriptor(&info) {
        array_descriptor_addr(b, &info)
    } else {
        materialize_array_descriptor_for_info(b, &info)
    };

    match name {
        "size" => {
            if args.len() >= 2 {
                // SIZE(array, dim)
                if let crate::ast::expr::SectionSubscript::Element(e) = &args[1].value {
                    let dim = lower_expr(b, locals, e, st);
                    let result64 = if local_uses_array_descriptor(&info) {
                        let desc = array_descriptor_addr(b, &info);
                        b.call(
                            FuncRef::External("afs_array_size_dim".into()),
                            vec![desc, dim],
                            IrType::Int(IntWidth::I64),
                        )
                    } else {
                        let raw_dim = match b.func().value_type(dim) {
                            Some(IrType::Int(IntWidth::I64)) => b.int_trunc(dim, IntWidth::I32),
                            _ => dim,
                        };
                        let one = b.const_i32(1);
                        let zero = b.const_i64(0);
                        let idx0 = b.isub(raw_dim, one);
                        let mut result = zero;
                        for (idx, (_lower, extent)) in info.dims.iter().enumerate() {
                            let cond_idx = b.const_i32(idx as i32);
                            let is_match = b.icmp(CmpOp::Eq, idx0, cond_idx);
                            let extent_val = b.const_i64(*extent);
                            result = b.select(is_match, extent_val, result);
                        }
                        result
                    };
                    Some(b.int_trunc(result64, IntWidth::I32))
                } else { None }
            } else {
                // SIZE(array)
                let result64 = if local_uses_array_descriptor(&info) {
                    let desc = array_descriptor_addr(b, &info);
                    b.call(
                        FuncRef::External("afs_array_size".into()),
                        vec![desc],
                        IrType::Int(IntWidth::I64),
                    )
                } else {
                    let total: i64 = info.dims.iter().map(|(_, extent)| *extent).product();
                    b.const_i64(total.max(0))
                };
                Some(b.int_trunc(result64, IntWidth::I32))
            }
        }
        "lbound" => {
            if args.len() >= 2 {
                if let crate::ast::expr::SectionSubscript::Element(e) = &args[1].value {
                    let dim = lower_expr(b, locals, e, st);
                    let result64 = if local_uses_array_descriptor(&info) {
                        let desc = array_descriptor_addr(b, &info);
                        b.call(
                            FuncRef::External("afs_array_lbound".into()),
                            vec![desc, dim],
                            IrType::Int(IntWidth::I64),
                        )
                    } else {
                        let raw_dim = match b.func().value_type(dim) {
                            Some(IrType::Int(IntWidth::I64)) => b.int_trunc(dim, IntWidth::I32),
                            _ => dim,
                        };
                        let one = b.const_i32(1);
                        let default = b.const_i64(1);
                        let idx0 = b.isub(raw_dim, one);
                        let mut result = default;
                        for (idx, (lower, _extent)) in info.dims.iter().enumerate() {
                            let cond_idx = b.const_i32(idx as i32);
                            let is_match = b.icmp(CmpOp::Eq, idx0, cond_idx);
                            let lower_val = b.const_i64(*lower);
                            result = b.select(is_match, lower_val, result);
                        }
                        result
                    };
                    Some(b.int_trunc(result64, IntWidth::I32))
                } else { None }
            } else { None }
        }
        "ubound" => {
            if args.len() >= 2 {
                if let crate::ast::expr::SectionSubscript::Element(e) = &args[1].value {
                    let dim = lower_expr(b, locals, e, st);
                    let result64 = if local_uses_array_descriptor(&info) {
                        let desc = array_descriptor_addr(b, &info);
                        b.call(
                            FuncRef::External("afs_array_ubound".into()),
                            vec![desc, dim],
                            IrType::Int(IntWidth::I64),
                        )
                    } else {
                        let raw_dim = match b.func().value_type(dim) {
                            Some(IrType::Int(IntWidth::I64)) => b.int_trunc(dim, IntWidth::I32),
                            _ => dim,
                        };
                        let one = b.const_i32(1);
                        let default = b.const_i64(0);
                        let idx0 = b.isub(raw_dim, one);
                        let mut result = default;
                        for (idx, (lower, extent)) in info.dims.iter().enumerate() {
                            let cond_idx = b.const_i32(idx as i32);
                            let is_match = b.icmp(CmpOp::Eq, idx0, cond_idx);
                            let upper_val = b.const_i64(lower + extent - 1);
                            result = b.select(is_match, upper_val, result);
                        }
                        result
                    };
                    Some(b.int_trunc(result64, IntWidth::I32))
                } else { None }
            } else { None }
        }
        "allocated" => {
            Some(b.call(FuncRef::External("afs_array_allocated".into()),
                vec![desc], IrType::Int(IntWidth::I32)))
        }
        "sum" => {
            let is_real = first_arg_is_real(args, locals);
            if is_real {
                Some(b.call(FuncRef::External("afs_array_sum_real8".into()),
                    vec![desc], IrType::Float(FloatWidth::F64)))
            } else {
                Some(b.call(FuncRef::External("afs_array_sum_int".into()),
                    vec![desc], IrType::Int(IntWidth::I64)))
            }
        }
        "product" => {
            let is_real = first_arg_is_real(args, locals);
            if is_real {
                Some(b.call(FuncRef::External("afs_array_product_real8".into()),
                    vec![desc], IrType::Float(FloatWidth::F64)))
            } else {
                Some(b.call(FuncRef::External("afs_array_product_int".into()),
                    vec![desc], IrType::Int(IntWidth::I64)))
            }
        }
        "maxval" => {
            let is_real = first_arg_is_real(args, locals);
            if is_real {
                Some(b.call(FuncRef::External("afs_array_maxval_real8".into()),
                    vec![desc], IrType::Float(FloatWidth::F64)))
            } else {
                Some(b.call(FuncRef::External("afs_array_maxval_int".into()),
                    vec![desc], IrType::Int(IntWidth::I32)))
            }
        }
        "minval" => {
            let is_real = first_arg_is_real(args, locals);
            if is_real {
                Some(b.call(FuncRef::External("afs_array_minval_real8".into()),
                    vec![desc], IrType::Float(FloatWidth::F64)))
            } else {
                Some(b.call(FuncRef::External("afs_array_minval_int".into()),
                    vec![desc], IrType::Int(IntWidth::I32)))
            }
        }
        "dot_product" => {
            let second_desc = args.get(1).and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    if let Expr::Name { name } = &e.node {
                        locals.get(&name.to_lowercase())
                            .filter(|i| local_uses_array_descriptor(i) || !i.dims.is_empty())
                            .map(|i| {
                                if local_uses_array_descriptor(i) {
                                    array_descriptor_addr(b, i)
                                } else {
                                    materialize_array_descriptor_for_info(b, i)
                                }
                            })
                    } else { None }
                } else { None }
            })?;
            // Get the first arg's element type for dispatch.
            let elem_ty = args.first().and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    if let Expr::Name { name } = &e.node {
                        locals.get(&name.to_lowercase()).map(|i| i.ty.clone())
                    } else { None }
                } else { None }
            }).unwrap_or(IrType::Float(FloatWidth::F64));
            match &elem_ty {
                IrType::Float(FloatWidth::F64) => Some(b.call(
                    FuncRef::External("afs_dot_product_real8".into()),
                    vec![desc, second_desc], IrType::Float(FloatWidth::F64))),
                IrType::Float(FloatWidth::F32) => Some(b.call(
                    FuncRef::External("afs_dot_product_real4".into()),
                    vec![desc, second_desc], IrType::Float(FloatWidth::F32))),
                _ => Some(b.call(
                    FuncRef::External("afs_dot_product_int".into()),
                    vec![desc, second_desc], IrType::Int(IntWidth::I64))),
            }
        }
        "matmul" => {
            // MATMUL(a, b) → allocate result descriptor, dispatch by type.
            let second_desc = args.get(1).and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    if let Expr::Name { name } = &e.node {
                        locals.get(&name.to_lowercase())
                            .filter(|i| local_uses_array_descriptor(i) || !i.dims.is_empty())
                            .map(|i| {
                                if local_uses_array_descriptor(i) {
                                    array_descriptor_addr(b, i)
                                } else {
                                    materialize_array_descriptor_for_info(b, i)
                                }
                            })
                    } else { None }
                } else { None }
            })?;
            let is_real = first_arg_is_real(args, locals);
            let result_desc = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384));
            let zero = b.const_i32(0);
            let sz384 = b.const_i64(384);
            b.call(FuncRef::External("memset".into()), vec![result_desc, zero, sz384],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            let func = if is_real { "afs_matmul_real8" } else { "afs_matmul_int" };
            b.call(FuncRef::External(func.into()),
                vec![desc, second_desc, result_desc], IrType::Void);
            Some(result_desc)
        }
        "transpose" => {
            // TRANSPOSE(source) → allocate result descriptor, dispatch by type.
            let is_real = first_arg_is_real(args, locals);
            let result_desc = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384));
            let zero = b.const_i32(0);
            let sz384 = b.const_i64(384);
            b.call(FuncRef::External("memset".into()), vec![result_desc, zero, sz384],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            let func = if is_real { "afs_transpose_real8" } else { "afs_transpose_int" };
            b.call(FuncRef::External(func.into()),
                vec![desc, result_desc], IrType::Void);
            Some(result_desc)
        }
        _ => None,
    }
}

/// Check if `actual_type` is or extends `target_type` (for CLASS IS matching).
fn is_type_or_extends(actual_type: &str, target_type: &str, tl: &crate::sema::type_layout::TypeLayoutRegistry) -> bool {
    if actual_type.eq_ignore_ascii_case(target_type) { return true; }
    // Walk the parent chain.
    let mut current = actual_type.to_lowercase();
    loop {
        let layout = match tl.get(&current) {
            Some(l) => l,
            None => return false,
        };
        match &layout.parent {
            Some(parent) if parent.eq_ignore_ascii_case(target_type) => return true,
            Some(parent) => current = parent.to_lowercase(),
            None => return false,
        }
    }
}

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
        16 => IrType::Int(IntWidth::I128),
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
            // For a derived-type POINTER, info.addr is a pointer slot
            // whose contents are the associated struct's address.
            // Dereference once to get the struct base.
            let addr = if info.is_pointer {
                b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
            } else if info.by_ref {
                b.load(info.addr)
            } else {
                info.addr
            };
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

fn lower_char_arg_by_ref(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> Option<ValueId> {
    use crate::ast::expr::Expr;

    match &expr.node {
        Expr::Name { name } => {
            let info = locals.get(&name.to_lowercase())?;
            if !info.dims.is_empty() {
                return None;
            }
            if info.by_ref
                && info.char_kind == CharKind::None
                && matches!(
                    info.ty,
                    IrType::Ptr(ref inner) if matches!(inner.as_ref(), IrType::Int(IntWidth::I8))
                )
            {
                return Some(info.addr);
            }
            let (ptr, _len) = char_addr_and_len(b, expr, locals)?;
            let slot = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            b.store(ptr, slot);
            Some(slot)
        }
        Expr::StringLiteral { value, .. } => {
            let src = b.const_string(value.as_bytes());
            let buf = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), (value.len() + 1) as u64));
            let zero = b.const_i32(0);
            let total = b.const_i64((value.len() + 1) as i64);
            b.call(
                FuncRef::External("memset".into()),
                vec![buf, zero, total],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
            );
            let len = b.const_i64(value.len() as i64);
            b.call(
                FuncRef::External("memcpy".into()),
                vec![buf, src, len],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
            );
            let zero_idx = b.const_i64(0);
            let ptr = b.gep(buf, vec![zero_idx], IrType::Int(IntWidth::I8));
            let slot = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            b.store(ptr, slot);
            Some(slot)
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let info = locals.get(&name.to_lowercase())?;
            if !matches!(info.char_kind, CharKind::Fixed(_)) || info.dims.is_empty() {
                return None;
            }
            let ptr = lower_array_element(b, locals, info, args, st);
            let slot = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            b.store(ptr, slot);
            Some(slot)
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
    if let Some(ptr_slot) = lower_char_arg_by_ref(b, locals, expr, st) {
        return ptr_slot;
    }
    // If it's a simple name, pass its address.
    if let Expr::Name { name } = &expr.node {
        let key = name.to_lowercase();
        if let Some(info) = locals.get(&key) {
            if info.by_ref {
                if info.descriptor_arg {
                    return array_data_ptr_for_call(b, info);
                }
                // Already a pointer to caller's storage — load and pass it.
                return b.load(info.addr);
            }
            if !info.dims.is_empty() || local_uses_array_descriptor(info) {
                return array_data_ptr_for_call(b, info);
            }
            return info.addr;
        }
    }
    // Array element: arr(i) passed by ref should pass the address
    // of the element within arr, NOT a copy.  This enables sequence
    // association (F2018 §15.5.2.11): a callee that declares a
    // larger dummy can walk to successive elements from the passed
    // address.
    if let Expr::FunctionCall { callee, args } = &expr.node {
        if let Expr::Name { name } = &callee.node {
            let key = name.to_lowercase();
            if let Some(info) = locals.get(&key) {
                if !info.dims.is_empty() || local_uses_array_descriptor(info) {
                    // Compute the element address via GEP.
                    if args.len() == 1 {
                        if let crate::ast::expr::SectionSubscript::Element(idx_expr) = &args[0].value {
                            let base = array_data_ptr_for_call(b, info);
                            let idx = lower_expr(b, locals, idx_expr, st);
                            let idx64 = match b.func().value_type(idx) {
                                Some(IrType::Int(IntWidth::I64)) => idx,
                                _ => b.int_extend(idx, IntWidth::I64, true),
                            };
                            let one = b.const_i64(1);
                            let idx0 = b.isub(idx64, one); // Fortran 1-indexed → 0-indexed
                            return b.gep(base, vec![idx0], info.ty.clone());
                        }
                    }
                }
            }
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
    lower_expr_full(b, locals, expr, st, None, None)
}

fn lower_expr_ctx(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    expr: &crate::ast::expr::SpannedExpr,
) -> ValueId {
    lower_expr_full(b, &ctx.locals, expr, ctx.st, None, Some(ctx.internal_funcs))
}

fn lower_expr_tl(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    tl: &crate::sema::type_layout::TypeLayoutRegistry,
) -> ValueId {
    lower_expr_full(b, locals, expr, st, Some(tl), None)
}

fn lower_expr_ctx_tl(
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
    )
}

fn lower_expr_full(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
    internal_funcs: Option<&HashMap<String, u32>>,
) -> ValueId {
    match &expr.node {
        Expr::IntegerLiteral { text, kind, .. } => {
            let kind = kind.as_deref();
            if kind == Some("16") {
                b.const_i128(text.parse::<i128>().unwrap_or(0))
            } else {
                let val: i64 = text.parse().unwrap_or(0);
                if kind == Some("8")
                    || val > i32::MAX as i64
                    || val < i32::MIN as i64
                {
                    b.const_i64(val)
                } else {
                    b.const_i32(val as i32)
                }
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
        Expr::BozLiteral { text, base } => {
            // BOZ literals: strip prefix letter and quotes, parse digit string.
            let radix = match base {
                crate::ast::expr::BozBase::Binary => 2,
                crate::ast::expr::BozBase::Octal => 8,
                crate::ast::expr::BozBase::Hex => 16,
            };
            // Token text is like Z'FF' or B'1010' — extract the digits between quotes.
            let digits: String = text.chars()
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
                } else if info.is_pointer && info.derived_type.is_none() {
                    // Scalar Fortran POINTER: `info.addr` is an alloca
                    // ptr<T>.  Reading the pointer as a value
                    // dereferences it: load the target address out of
                    // the slot, then load the value through it.
                    let tgt = b.load_typed(info.addr, IrType::Ptr(Box::new(info.ty.clone())));
                    b.load_typed(tgt, info.ty.clone())
                } else if info.is_pointer && info.derived_type.is_some() {
                    // Derived-type POINTER used as a bare Name (e.g.
                    // passed to a subroutine expecting type(t)).  The
                    // consumer wants the struct address, which is
                    // what's stored in the pointer slot.
                    b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                } else if info.derived_type.is_some() {
                    // Derived type variable: storage is `alloca [i8 x size]`.
                    // Consumers of the value treat it as a pointer to the
                    // struct (memcpy for whole-struct assignment, GEP for
                    // component access). Without this case we fell through
                    // to load_typed(info.ty) which yanked the first 8 bytes
                    // of the struct as if they were a pointer, turning
                    // `b = a` into a memcpy from the garbage address held
                    // by a's first field slot.
                    if info.by_ref { b.load(info.addr) } else { info.addr }
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
                b.const_i32(0)
            }
        }

        Expr::BinaryOp { op, left, right } => {
            let mut lhs = lower_expr_full(b, locals, left, st, type_layouts, internal_funcs);
            let mut rhs = lower_expr_full(b, locals, right, st, type_layouts, internal_funcs);
            let lty = b.func().value_type(lhs).unwrap_or(IrType::Int(IntWidth::I32));
            let rty = b.func().value_type(rhs).unwrap_or(IrType::Int(IntWidth::I32));

            // Complex arithmetic: both operands are ptr<[f32/f64 x 2]>.
            // Add/Sub operate component-wise; Mul uses (ac-bd, ad+bc).
            if is_complex_ty(&lty) || is_complex_ty(&rty) {
                let fw = if complex_float_width(&lty) == FloatWidth::F64
                    || complex_float_width(&rty) == FloatWidth::F64
                {
                    FloatWidth::F64
                } else {
                    FloatWidth::F32
                };
                let elem = IrType::Float(fw);
                let esz = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                let zero = b.const_i64(0);
                // Load components from lhs (re_l, im_l).
                let re_l_ptr = b.gep(lhs, vec![zero], IrType::Int(IntWidth::I8));
                let im_l_ptr = b.gep(lhs, vec![esz], IrType::Int(IntWidth::I8));
                let re_l = b.load_typed(re_l_ptr, elem.clone());
                let im_l = b.load_typed(im_l_ptr, elem.clone());
                // Load components from rhs (re_r, im_r).
                let re_r_ptr = b.gep(rhs, vec![zero], IrType::Int(IntWidth::I8));
                let im_r_ptr = b.gep(rhs, vec![esz], IrType::Int(IntWidth::I8));
                let re_r = b.load_typed(re_r_ptr, elem.clone());
                let im_r = b.load_typed(im_r_ptr, elem.clone());
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
            let val = lower_expr_full(b, locals, operand, st, type_layouts, internal_funcs);
            let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
            match (op, &ty) {
                (UnaryOp::Minus, IrType::Int(_)) => b.ineg(val),
                (UnaryOp::Minus, IrType::Float(_)) => b.fneg(val),
                (UnaryOp::Plus, _) => val,
                (UnaryOp::Not, _) => b.not(val),
                _ => val,
            }
        }

        Expr::ParenExpr { inner } => lower_expr_full(b, locals, inner, st, type_layouts, internal_funcs),

        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();

                // Check if this is an array element or section access.
                if let Some(info) = locals.get(&key) {
                    if !info.dims.is_empty() || info.allocatable {
                        let has_range = args.iter().any(|a| matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. }));
                        if has_range {
                            return lower_array_section(b, locals, info, args, st);
                        }
                        return lower_array_element(b, locals, info, args, st);
                    }
                }

                // Check for pointer intrinsics (ASSOCIATED) first —
                // these work on every pointer shape and don't care
                // about the array-intrinsic filter.
                if let Some(result) = lower_pointer_intrinsic(b, locals, &key, args) {
                    return result;
                }

                // Check for array intrinsics (SIZE, SUM, etc.) that need descriptor addresses.
                if let Some(result) = lower_array_intrinsic(b, locals, &key, args, st) {
                    return result;
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
                                    let val = lower_expr_full(b, locals, e, st, type_layouts, internal_funcs);
                                    let coerced = coerce_to_type(
                                        b,
                                        val,
                                        &type_info_to_ir_type(&layout.fields[i].type_info),
                                    );
                                    let offset = b.const_i64(layout.fields[i].offset as i64);
                                    let field_ptr = b.gep(tmp, vec![offset], IrType::Int(IntWidth::I8));
                                    b.store(coerced, field_ptr);
                                }
                            }
                        }
                        return tmp;
                    }
                }

                // Try character intrinsics (need access to locals for CharKind).
                if let Some(result) = lower_char_intrinsic(b, &key, args, locals, st) {
                    return result;
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

                // Try intrinsic lowering first (intrinsics use values, not references).
                let intrinsic_arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => {
                            lower_expr_full(b, locals, e, st, type_layouts, internal_funcs)
                        }
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
                                lower_expr_full(b, locals, e, st, type_layouts, internal_funcs)
                            } else {
                                lower_arg_by_ref(b, locals, e, st)
                            }
                        }
                        _ => b.const_i32(0),
                    }
                }).collect();

                // Look up callee return type from symbol table.
                // Search all scopes since the current scope may be global after resolve.
                let ret_ty = callee_return_ir_type(st, &key).unwrap_or(IrType::Int(IntWidth::I32));
                let func_ref = internal_funcs
                    .and_then(|map| map.get(&key).copied())
                    .map(FuncRef::Internal)
                    .unwrap_or_else(|| FuncRef::External(name.clone()));
                b.call(func_ref, ref_arg_vals, ret_ty)
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
            let elem_ty = values.iter()
                .find_map(|v| match v {
                    crate::ast::expr::AcValue::Expr(e) => {
                        // Peek at the first element's type by
                        // lowering it on a scratch path. Rather
                        // than actually lower (and have to undo),
                        // approximate from the AST: integer
                        // literals → i32, real → f64, etc.
                        Some(infer_const_expr_ty(&e.node))
                    }
                    _ => None,
                })
                .unwrap_or(IrType::Int(IntWidth::I32));
            let n = values.len() as u64;
            let arr_ty = IrType::Array(Box::new(elem_ty.clone()), n.max(1));
            let buf = b.alloca(arr_ty);
            store_ac_values_into(b, locals, buf, &elem_ty, values, st);
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

            let real_raw = lower_expr_full(b, locals, real, st, type_layouts, internal_funcs);
            let imag_raw = lower_expr_full(b, locals, imag, st, type_layouts, internal_funcs);
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

/// Approximate the IR type of a constant-or-near-constant
/// expression by inspecting the AST. Used by ArrayConstructor
/// lowering to pick an element type without actually emitting IR.
/// Conservative — falls back to i32 for anything it can't
/// classify.
fn infer_const_expr_ty(e: &Expr) -> IrType {
    match e {
        Expr::IntegerLiteral { kind, .. } => {
            if kind.as_deref() == Some("16") {
                IrType::Int(IntWidth::I128)
            } else if kind.as_deref() == Some("8") {
                IrType::Int(IntWidth::I64)
            } else {
                IrType::Int(IntWidth::I32)
            }
        }
        Expr::RealLiteral { text, .. } => {
            if text.to_lowercase().contains('d') {
                IrType::Float(FloatWidth::F64)
            } else {
                IrType::Float(FloatWidth::F32)
            }
        }
        Expr::LogicalLiteral { .. } => IrType::Bool,
        Expr::UnaryOp { operand, .. } => infer_const_expr_ty(&operand.node),
        Expr::ParenExpr { inner } => infer_const_expr_ty(&inner.node),
        _ => IrType::Int(IntWidth::I32),
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
    fn lower_print_integer16_uses_wide_writer() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer(16) :: x
  x = 170141183460469231731687303715884105727_16
  print *, x
end program
");
        assert!(ir.contains("afs_write_int128"));
    }

    #[test]
    fn lower_read_integer16_uses_wide_reader() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer(16) :: x
  read(*, *) x
end program
");
        assert!(ir.contains("afs_read_int128"));
    }

    #[test]
    fn lower_internal_write_integer16_uses_wide_buffer_writer() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  character(len=96) :: buf
  integer(16) :: x
  x = 170141183460469231731687303715884105727_16
  write(buf, *) x
end program
");
        assert!(ir.contains("afs_write_internal_int128"));
    }

    #[test]
    fn lower_internal_read_integer16_uses_wide_buffer_reader() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  character(len=96) :: buf
  integer(16) :: x
  read(buf, *) x
end program
");
        assert!(ir.contains("afs_read_internal_int128"));
    }

    #[test]
    fn lower_formatted_internal_write_integer16_uses_internal_format_sink() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  character(len=96) :: buf
  integer(16) :: x
  x = 170141183460469231731687303715884105727_16
  write(buf, '(I40)') x
end program
");
        assert!(ir.contains("afs_fmt_begin_internal"));
        assert!(ir.contains("afs_fmt_push_int128"));
    }

    #[test]
    fn lower_formatted_internal_read_integer16_uses_internal_format_reader() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  character(len=64) :: buf
  integer(16) :: x
  read(buf, '(I40)') x
end program
");
        assert!(ir.contains("afs_fmt_read_int128_internal"));
    }

    #[test]
    fn lower_formatted_read_integer16_uses_wide_format_reader() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer(16) :: x
  read(10, '(I40)') x
end program
");
        assert!(ir.contains("afs_fmt_read_int128"));
    }

    #[test]
    fn lower_formatted_write_integer16_uses_wide_push() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer(16) :: x
  x = 170141183460469231731687303715884105727_16
  write(*, '(I40)') x
end program
");
        assert!(ir.contains("afs_fmt_push_int128"));
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
        // Simple diamond `if (cond) y = a; else y = b` lowers to Select.
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
        assert!(ir.contains("select"), "simple diamond should lower to select: {}", ir);
    }

    #[test]
    fn lower_if_then_else_branching() {
        // Non-diamond IF/ELSE (multi-statement body) must still use branches.
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y, z
  x = 5
  if (x > 0) then
    y = 1
    z = 2
  else
    y = -1
    z = -2
  end if
end program
");
        assert!(ir.contains("cond_br"), "multi-stmt if/else should use branches: {}", ir);
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
    fn lower_do_concurrent_uses_distinct_blocks() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, arr(10)
  do concurrent (i = 1:10)
    arr(i) = i * 2
  end do
end program
");
        assert!(ir.contains("doconc_check"));
        assert!(ir.contains("doconc_body"));
        assert!(ir.contains("doconc_incr"));
        assert!(ir.contains("doconc_exit"));
        assert!(ir.contains("icmp le"));
    }

    #[test]
    fn lower_do_concurrent_mask_emits_guard() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, arr(6)
  arr = 0
  do concurrent (i = 1:6, mod(i, 2) == 0)
    arr(i) = i
  end do
end program
");
        assert!(ir.contains("doconc_check"));
        assert!(ir.contains("if_then"));
        assert!(ir.contains("if_end"));
    }

    #[test]
    fn lower_do_concurrent_multiple_controls_nests_loops() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, j, arr(3, 2)
  do concurrent (i = 1:3, j = 1:2)
    arr(i, j) = i * 10 + j
  end do
end program
");
        assert!(ir.matches("doconc_check").count() >= 2);
    }

    #[test]
    fn lower_do_concurrent_full_array_map_uses_bulk_kernel() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, a(8), b(8), c(8)
  do i = 1, 8
    a(i) = i
    b(i) = i * 10
  end do
  do concurrent (i = 1:8)
    c(i) = a(i) + b(i)
  end do
end program
");
        assert!(ir.contains("call @afs_array_add_i32("));
        assert!(!ir.contains("doconc_check"));
    }

    #[test]
    fn lower_whole_array_elemental_assign_uses_do_concurrent_shape() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: a(4), b(4), i
  do i = 1, 4
    a(i) = i * 2
  end do
  b = shift_scale(a, 5)
contains
  elemental function shift_scale(x, y) result(r)
    integer, intent(in) :: x, y
    integer :: r
    r = x * 2 + y
  end function
end program
");
        assert!(ir.contains("doconc_check"));
        assert!(ir.contains("call @shift_scale("));
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
        assert!(ir.contains("call @afs_allocate_array"), "expected allocate call in:\n{}", ir);
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
