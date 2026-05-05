//! IR instructions, terminators, and values.
//!
//! Every instruction produces a value (ValueId) in SSA form.
//! Basic blocks end with exactly one Terminator.

use super::types::{FloatWidth, IntWidth, IrType};
use crate::lexer::Span;
use std::collections::HashMap;

/// A value identifier — unique within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(pub u32);

/// A basic block identifier — unique within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// A function reference — index into Module::functions or external.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FuncRef {
    /// Index into Module::functions.
    Internal(u32),
    /// External function by name (runtime calls, etc.).
    External(String),
    /// Indirect call through a pointer-typed SSA value.
    Indirect(ValueId),
}

/// A runtime library function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuntimeFunc {
    PrintInt,
    PrintReal,
    PrintString,
    PrintLogical,
    PrintNewline,
    Allocate,
    Deallocate,
    StringConcat,
    StringCopy,
    StringCompare,
    Stop,
    ErrorStop,
    /// Bounds check: `afs_check_bounds(index, lower_bound, upper_bound)`.
    /// Aborts if index < lower or index > upper. Inserted at array access
    /// sites; eliminated at O2+ when provably safe.
    CheckBounds,
}

/// Comparison operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// An SSA instruction.
#[derive(Debug, Clone)]
pub struct Inst {
    pub id: ValueId,
    pub kind: InstKind,
    pub ty: IrType,
    pub span: Span,
}

/// Instruction kinds.
#[derive(Debug, Clone)]
pub enum InstKind {
    // ---- Constants ----
    ConstInt(i128, IntWidth),
    ConstFloat(f64, FloatWidth),
    ConstBool(bool),
    ConstString(Vec<u8>),
    Undef(IrType),

    // ---- Integer arithmetic ----
    IAdd(ValueId, ValueId),
    ISub(ValueId, ValueId),
    IMul(ValueId, ValueId),
    IDiv(ValueId, ValueId),
    IMod(ValueId, ValueId),
    INeg(ValueId),

    // ---- Float arithmetic ----
    FAdd(ValueId, ValueId),
    FSub(ValueId, ValueId),
    FMul(ValueId, ValueId),
    FDiv(ValueId, ValueId),
    FNeg(ValueId),
    FAbs(ValueId),
    FSqrt(ValueId),
    FPow(ValueId, ValueId),

    // ---- Comparison ----
    ICmp(CmpOp, ValueId, ValueId),
    FCmp(CmpOp, ValueId, ValueId),

    // ---- Logic ----
    And(ValueId, ValueId),
    Or(ValueId, ValueId),
    Not(ValueId),

    // ---- Select (conditional) ----
    /// Select(cond, true_val, false_val) → cond ? true_val : false_val
    Select(ValueId, ValueId, ValueId),

    // ---- Bitwise ----
    BitAnd(ValueId, ValueId),
    BitOr(ValueId, ValueId),
    BitXor(ValueId, ValueId),
    BitNot(ValueId),
    Shl(ValueId, ValueId),
    LShr(ValueId, ValueId),
    AShr(ValueId, ValueId),
    CountLeadingZeros(ValueId),
    CountTrailingZeros(ValueId),
    PopCount(ValueId),

    // ---- Conversions ----
    IntToFloat(ValueId, FloatWidth),
    FloatToInt(ValueId, IntWidth),
    FloatExtend(ValueId, FloatWidth),
    FloatTrunc(ValueId, FloatWidth),
    IntExtend(ValueId, IntWidth, bool), // bool = signed
    IntTrunc(ValueId, IntWidth),
    /// Convert a pointer to an integer (address as i64).
    PtrToInt(ValueId),
    /// Convert an integer (i64 address) to a pointer.
    IntToPtr(ValueId, IrType),

    // ---- Memory ----
    Alloca(IrType),
    Load(ValueId),
    Store(ValueId, ValueId),              // store(value, addr)
    GetElementPtr(ValueId, Vec<ValueId>), // base, indices
    /// Address of a module-level global. Returns `Ptr<T>` where T
    /// is the global's declared type. Used by SAVE'd locals (those
    /// with initializers in subprograms) and module variables —
    /// any storage that must persist across function calls.
    GlobalAddr(String),

    // ---- Calls ----
    Call(FuncRef, Vec<ValueId>),
    RuntimeCall(RuntimeFunc, Vec<ValueId>),

    // ---- Aggregates ----
    ExtractField(ValueId, u32),
    InsertField(ValueId, u32, ValueId),

    // ---- SIMD vector ops (Sprint 12 Stage 1) ----
    //
    // All vector ops operate on `IrType::Vector` values (128-bit NEON).
    // Element-wise binary / unary arithmetic and bitwise ops follow the
    // same lane shape as their operands. `VLoad`/`VStore` move 128 bits
    // to / from a `Ptr<elem>`. `VBroadcast` splats a scalar across every
    // lane. `VExtract` / `VInsert` access a single lane by constant
    // index (lane index is `u8`, not a `ValueId`, so SCCP and
    // const-fold can reason about it). Reductions take a vector and
    // return a scalar.
    VAdd(ValueId, ValueId),
    VSub(ValueId, ValueId),
    VMul(ValueId, ValueId),
    VDiv(ValueId, ValueId),
    VNeg(ValueId),
    VAbs(ValueId),
    VSqrt(ValueId),
    VFma(ValueId, ValueId, ValueId), // a*b + c
    VMin(ValueId, ValueId),
    VMax(ValueId, ValueId),
    VICmp(CmpOp, ValueId, ValueId),
    VFCmp(CmpOp, ValueId, ValueId),
    VLoad(ValueId),                 // ptr → vector
    VStore(ValueId, ValueId),       // (vector, ptr)
    VBitcast(ValueId, IrType),      // reinterpret element layout
    VExtract(ValueId, u8),          // extract lane `n`
    VInsert(ValueId, u8, ValueId),  // (vector, lane, scalar) → vector
    VBroadcast(ValueId),            // scalar → vector (every lane)
    VReduceSum(ValueId),            // vector → scalar (cross-lane sum)
    VReduceMin(ValueId),            // vector → scalar (cross-lane min)
    VReduceMax(ValueId),            // vector → scalar (cross-lane max)
}

/// Block terminator — exactly one per basic block.
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Return from function.
    Return(Option<ValueId>),
    /// Unconditional branch with block arguments.
    Branch(BlockId, Vec<ValueId>),
    /// Conditional branch: condition, true target + args, false target + args.
    CondBranch {
        cond: ValueId,
        true_dest: BlockId,
        true_args: Vec<ValueId>,
        false_dest: BlockId,
        false_args: Vec<ValueId>,
    },
    /// Multi-way branch (SELECT CASE).
    Switch {
        selector: ValueId,
        cases: Vec<(i64, BlockId)>,
        default: BlockId,
    },
    /// Unreachable — after a STOP or ERROR STOP.
    Unreachable,
}

/// A block parameter (replaces phi nodes).
#[derive(Debug, Clone)]
pub struct BlockParam {
    pub id: ValueId,
    pub ty: IrType,
}

/// A basic block in a function.
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub name: String,
    pub params: Vec<BlockParam>,
    pub insts: Vec<Inst>,
    pub terminator: Option<Terminator>,
}

impl BasicBlock {
    pub fn new(id: BlockId, name: String) -> Self {
        Self {
            id,
            name,
            params: Vec::new(),
            insts: Vec::new(),
            terminator: None,
        }
    }
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: IrType,
    pub id: ValueId,
    /// Fortran dummy arguments are non-aliasing by default unless
    /// lowered from POINTER/TARGET-like declarations.
    pub fortran_noalias: bool,
}

/// A function in the IR module.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: IrType,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    next_value: u32,
    next_block: u32,
    /// O(1) type lookup cache. Populated during construction; call
    /// `rebuild_type_cache()` after optimizer passes mutate the IR.
    type_cache: HashMap<ValueId, IrType>,
    /// Fortran PURE attribute — function has no side effects.
    pub is_pure: bool,
    /// Fortran ELEMENTAL attribute — operates element-wise on arrays.
    pub is_elemental: bool,
    /// True for contained procedures that never participate in the
    /// external object ABI and can be rewritten by IPO passes.
    pub internal_only: bool,
}

impl Function {
    pub fn new(name: String, params: Vec<Param>, return_type: IrType) -> Self {
        let entry = BlockId(0);
        let entry_block = BasicBlock::new(entry, "entry".into());
        let next_value = params.iter().map(|p| p.id.0 + 1).max().unwrap_or(0);
        let mut type_cache = HashMap::new();
        for p in &params {
            type_cache.insert(p.id, p.ty.clone());
        }
        Self {
            name,
            params,
            return_type,
            blocks: vec![entry_block],
            entry,
            next_value,
            next_block: 1,
            type_cache,
            is_pure: false,
            is_elemental: false,
            internal_only: false,
        }
    }

    /// Allocate a fresh ValueId.
    pub fn next_value_id(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    /// Allocate a fresh BlockId and create the block.
    /// Appends the block ID to ensure unique label names.
    pub fn create_block(&mut self, name: &str) -> BlockId {
        let id = BlockId(self.next_block);
        self.next_block += 1;
        let unique_name = format!("{}_{}", name, id.0);
        self.blocks.push(BasicBlock::new(id, unique_name));
        id
    }

    /// Get a block by ID. Panics if the ID is not present — use
    /// `try_block` for graceful degradation.
    pub fn block(&self, id: BlockId) -> &BasicBlock {
        self.blocks
            .iter()
            .find(|b| b.id == id)
            .expect("block not found")
    }

    /// Get a mutable block by ID. Panics if the ID is not present.
    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        self.blocks
            .iter_mut()
            .find(|b| b.id == id)
            .expect("block not found")
    }

    /// Get a block by ID, returning `None` if the ID is not
    /// present. Audit N-10: used by CFG walks that may follow a
    /// terminator to a target that was just pruned mid-pass. The
    /// verifier rejects dangling targets, so on valid IR this
    /// behaves like `block`, but optimizer passes that intentionally
    /// run before block pruning (or that mutate the CFG) can use
    /// this to degrade gracefully instead of panicking.
    pub fn try_block(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// Register a value's type in the O(1) lookup cache.
    /// Called by FuncBuilder during construction.
    pub fn register_type(&mut self, id: ValueId, ty: IrType) {
        self.type_cache.insert(id, ty);
    }

    /// Rebuild the type cache from scratch. Call after optimizer passes
    /// that add/remove/rewrite instructions.
    pub fn rebuild_type_cache(&mut self) {
        self.type_cache.clear();
        for p in &self.params {
            self.type_cache.insert(p.id, p.ty.clone());
        }
        for block in &self.blocks {
            for bp in &block.params {
                self.type_cache.insert(bp.id, bp.ty.clone());
            }
            for inst in &block.insts {
                self.type_cache.insert(inst.id, inst.ty.clone());
            }
        }
    }

    /// Get the type of a value by ID. O(1) via cache.
    pub fn value_type(&self, id: ValueId) -> Option<IrType> {
        if let Some(ty) = self.type_cache.get(&id) {
            return Some(ty.clone());
        }
        // Cache miss — the authoritative source is the instruction or
        // parameter that defines the value. An earlier version of the
        // verifier silently skipped checks whenever the cache lagged
        // behind optimiser passes, which hid width mismatches and
        // pointer-type bugs for entire compilation units. Recompute
        // on-demand so consumers always get a consistent answer.
        for p in &self.params {
            if p.id == id {
                return Some(p.ty.clone());
            }
        }
        for block in &self.blocks {
            for bp in &block.params {
                if bp.id == id {
                    return Some(bp.ty.clone());
                }
            }
            for inst in &block.insts {
                if inst.id == id {
                    return Some(inst.ty.clone());
                }
            }
        }
        None
    }

    /// Find the instruction that defines a value, searching all blocks.
    pub fn find_defining_inst(&self, id: ValueId) -> Option<&Inst> {
        for block in &self.blocks {
            for inst in &block.insts {
                if inst.id == id {
                    return Some(inst);
                }
            }
        }
        None
    }
}

/// A global variable.
#[derive(Debug, Clone)]
pub struct Global {
    pub name: String,
    pub ty: IrType,
    pub initializer: Option<GlobalInit>,
}

/// Global variable initializer.
#[derive(Debug, Clone)]
pub enum GlobalInit {
    Zero,
    Int(i128),
    Float(f64),
    String(Vec<u8>),
    /// Array literal: a sequence of per-element initializers of
    /// the same homogeneous element type. Used by module array
    /// variables with `[v0, v1, ...]` or `(/ v0, v1, ... /)`
    /// constructors. The Vec's length is the array's total element
    /// count (product of dims); shorter initializers are padded
    /// with Zero at lowering time.
    IntArray(Vec<i128>),
    FloatArray(Vec<f64>),
}

/// The top-level IR module.
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub globals: Vec<Global>,
    pub functions: Vec<Function>,
    pub struct_defs: Vec<super::types::StructDef>,
    pub extern_funcs: Vec<ExternFunc>,
}

/// An external function declaration.
#[derive(Debug, Clone)]
pub struct ExternFunc {
    pub name: String,
    pub sig: super::types::FuncSig,
}

impl Module {
    pub fn new(name: String) -> Self {
        Self {
            name,
            globals: Vec::new(),
            functions: Vec::new(),
            struct_defs: Vec::new(),
            extern_funcs: Vec::new(),
        }
    }

    /// Add a function and return its index.
    pub fn add_function(&mut self, func: Function) -> u32 {
        let idx = self.functions.len() as u32;
        self.functions.push(func);
        idx
    }

    /// Add a global variable.
    pub fn add_global(&mut self, global: Global) {
        self.globals.push(global);
    }

    /// Add a struct definition and return its ID.
    pub fn add_struct(&mut self, def: super::types::StructDef) -> super::types::StructId {
        let id = self.struct_defs.len() as u32;
        self.struct_defs.push(def);
        id
    }

    /// True when any live IR surface in the module uses `i128`.
    pub fn contains_i128(&self) -> bool {
        self.globals
            .iter()
            .any(|global| type_contains_i128(self, &global.ty))
            || self
                .extern_funcs
                .iter()
                .any(|func| sig_contains_i128(self, &func.sig))
            || self.functions.iter().any(|func| {
                type_contains_i128(self, &func.return_type)
                    || func
                        .params
                        .iter()
                        .any(|param| type_contains_i128(self, &param.ty))
                    || func.blocks.iter().any(|block| {
                        block
                            .params
                            .iter()
                            .any(|param| type_contains_i128(self, &param.ty))
                            || block
                                .insts
                                .iter()
                                .any(|inst| type_contains_i128(self, &inst.ty))
                    })
            })
    }

    /// True when `i128` appears anywhere outside module-global storage.
    ///
    /// Globals-only `i128` is the first staged backend surface we support:
    /// the optimizer may ignore it and the emitter can lay it out as raw data.
    /// Parameters, returns, instruction results, block params, and extern
    /// signatures still imply unsupported ABI or codegen work.
    pub fn contains_i128_outside_globals(&self) -> bool {
        self.extern_funcs
            .iter()
            .any(|func| sig_contains_i128(self, &func.sig))
            || self.functions.iter().any(|func| {
                type_contains_i128(self, &func.return_type)
                    || func
                        .params
                        .iter()
                        .any(|param| type_contains_i128(self, &param.ty))
                    || func.blocks.iter().any(|block| {
                        block
                            .params
                            .iter()
                            .any(|param| type_contains_i128(self, &param.ty))
                            || block
                                .insts
                                .iter()
                                .any(|inst| type_contains_i128(self, &inst.ty))
                    })
            })
    }

    /// True when every `i128` use in the module is a global data shape that
    /// the current backend emitter can lay out directly.
    pub fn i128_backend_data_only_supported(&self) -> bool {
        !self.contains_i128_outside_globals()
            && self
                .globals
                .iter()
                .all(|global| global_i128_backend_data_supported(self, global))
    }

    /// True when the current O0 backend can lower every live `i128` surface.
    ///
    /// Today that means:
    /// - module-global `i128` data
    /// - local `i128` allocas and plain memory traffic around them
    /// - `i128` constants that only flow through those memory ops
    /// - stack-backed `i128` SSA block params and edge copies introduced
    ///   by mem2reg-style promotion
    /// - local `i128` selects that stay entirely within stack-backed values
    /// - direct `i128` params/returns and stack/register direct calls
    ///
    /// It still excludes runtime-call `i128`, broad optimized-pipeline
    /// support beyond the staged O1 lane, and any integer operation
    /// whose result or operands are `i128` beyond the currently lowered
    /// stack-backed surface.
    pub fn i128_backend_o0_supported(&self) -> bool {
        self.globals
            .iter()
            .all(|global| global_i128_backend_data_supported(self, global))
            && self.extern_funcs.iter().all(|func| {
                abi_type_i128_backend_o0_supported(self, &func.sig.ret, true)
                    && func
                        .sig
                        .params
                        .iter()
                        .all(|param| abi_type_i128_backend_o0_supported(self, param, true))
            })
            && self
                .functions
                .iter()
                .all(|func| function_i128_backend_o0_supported(self, func))
    }
}

fn sig_contains_i128(module: &Module, sig: &super::types::FuncSig) -> bool {
    type_contains_i128(module, &sig.ret)
        || sig
            .params
            .iter()
            .any(|param| type_contains_i128(module, param))
}

fn type_contains_i128(module: &Module, ty: &IrType) -> bool {
    match ty {
        IrType::Int(IntWidth::I128) => true,
        IrType::Ptr(inner) | IrType::Array(inner, _) => type_contains_i128(module, inner),
        IrType::Struct(id) => module.struct_defs.get(*id as usize).is_some_and(|def| {
            def.fields
                .iter()
                .any(|(_, field_ty)| type_contains_i128(module, field_ty))
        }),
        IrType::FuncPtr(sig) => sig_contains_i128(module, sig),
        _ => false,
    }
}

fn global_i128_backend_data_supported(module: &Module, global: &Global) -> bool {
    match &global.ty {
        IrType::Int(IntWidth::I128) => matches!(
            global.initializer,
            Some(GlobalInit::Int(_)) | Some(GlobalInit::Zero) | None
        ),
        IrType::Array(elem_ty, _) if matches!(elem_ty.as_ref(), IrType::Int(IntWidth::I128)) => {
            matches!(
                global.initializer,
                Some(GlobalInit::IntArray(_)) | Some(GlobalInit::Zero) | None
            )
        }
        _ => !type_contains_i128(module, &global.ty),
    }
}

fn abi_type_i128_backend_o0_supported(
    module: &Module,
    ty: &IrType,
    allow_direct_i128_scalar: bool,
) -> bool {
    match ty {
        IrType::Int(IntWidth::I128) => allow_direct_i128_scalar,
        IrType::Ptr(_) => true,
        _ => !type_contains_i128(module, ty),
    }
}

fn ssa_type_i128_backend_o0_supported(module: &Module, ty: &IrType) -> bool {
    matches!(ty, IrType::Int(IntWidth::I128))
        || abi_type_i128_backend_o0_supported(module, ty, false)
}

fn direct_call_i128_backend_o0_supported(
    module: &Module,
    func: &Function,
    callee: &FuncRef,
    args: &[ValueId],
    result_ty: &IrType,
) -> bool {
    // Indirect calls share the same arg-passing path as direct calls at
    // the codegen level — the only difference is how the callee address
    // is materialised. Accept all three variants for the i128 stack-slot
    // surface so an i128-typed actual passed through a procedure-pointer
    // component (e.g. process_type's `payload : class(*), pointer` slot
    // in stdlib_system_subprocess) doesn't trip the support gate.
    matches!(
        callee,
        FuncRef::Internal(_) | FuncRef::External(_) | FuncRef::Indirect(_)
    ) && abi_type_i128_backend_o0_supported(module, result_ty, true)
        && args
            .iter()
            .filter_map(|arg| func.value_type(*arg))
            .all(|ty| abi_type_i128_backend_o0_supported(module, &ty, true))
}

fn runtime_call_i128_backend_o0_supported(
    module: &Module,
    func: &Function,
    rf: &RuntimeFunc,
    args: &[ValueId],
    result_ty: &IrType,
) -> bool {
    match rf {
        RuntimeFunc::PrintInt => {
            matches!(result_ty, IrType::Void)
                && args.len() == 1
                && args
                    .iter()
                    .filter_map(|arg| func.value_type(*arg))
                    .all(|ty| abi_type_i128_backend_o0_supported(module, &ty, true))
        }
        _ => false,
    }
}

fn function_i128_backend_o0_supported(module: &Module, func: &Function) -> bool {
    if !abi_type_i128_backend_o0_supported(module, &func.return_type, true)
        || func
            .params
            .iter()
            .any(|param| !abi_type_i128_backend_o0_supported(module, &param.ty, true))
    {
        return false;
    }

    func.blocks.iter().all(|block| {
        block
            .params
            .iter()
            .all(|param| ssa_type_i128_backend_o0_supported(module, &param.ty))
            && block
                .insts
                .iter()
                .all(|inst| inst_i128_backend_o0_supported(module, func, inst))
            && block
                .terminator
                .as_ref()
                .is_none_or(|term| terminator_i128_backend_o0_supported(module, func, term))
    })
}

fn inst_i128_backend_o0_supported(module: &Module, func: &Function, inst: &Inst) -> bool {
    let inst_ty_has_i128 = type_contains_i128(module, &inst.ty);
    let uses_i128 = crate::ir::walk::inst_uses(&inst.kind)
        .into_iter()
        .filter_map(|vid| func.value_type(vid))
        .any(|ty| type_contains_i128(module, &ty));

    match &inst.kind {
        InstKind::ConstInt(_, IntWidth::I128) => true,
        InstKind::Undef(_) if matches!(inst.ty, IrType::Int(IntWidth::I128)) => true,
        InstKind::Load(_) if matches!(inst.ty, IrType::Int(IntWidth::I128)) => true,
        // Loading a *pointer* whose pointee happens to be i128 is just
        // an 8-byte vreg load — the wide-slot machinery isn't involved.
        // type_contains_i128 walks through Ptr<>, so without this arm
        // the catch-all would falsely reject `load ptr<i128>` when
        // dereferencing an sret-style hidden result-buffer pointer.
        InstKind::Load(_) if matches!(inst.ty, IrType::Ptr(_)) => true,
        InstKind::IAdd(..) | InstKind::ISub(..) | InstKind::INeg(_)
            if matches!(inst.ty, IrType::Int(IntWidth::I128)) =>
        {
            true
        }
        InstKind::ICmp(..) if uses_i128 => true,
        InstKind::Select(..) if matches!(inst.ty, IrType::Int(IntWidth::I128)) => true,
        InstKind::Call(callee, args) if inst_ty_has_i128 || uses_i128 => {
            direct_call_i128_backend_o0_supported(module, func, callee, args, &inst.ty)
        }
        InstKind::RuntimeCall(rf, args) if inst_ty_has_i128 || uses_i128 => {
            runtime_call_i128_backend_o0_supported(module, func, rf, args, &inst.ty)
        }
        InstKind::Store(..) => true,
        // Address-producing ops are safe even when they walk storage that
        // contains i128. The widened backend already knows how to carry the
        // actual loads/stores/calls that touch the value; rejecting the byte
        // cursor itself falsely blocks legal array-constructor and component
        // lvalue paths.
        InstKind::Alloca(_) | InstKind::GlobalAddr(_) | InstKind::GetElementPtr(..) => true,
        _ => !inst_ty_has_i128 && !uses_i128,
    }
}

fn terminator_i128_backend_o0_supported(
    module: &Module,
    func: &Function,
    term: &Terminator,
) -> bool {
    let term_uses_i128 = crate::ir::walk::terminator_uses(term)
        .into_iter()
        .filter_map(|vid| func.value_type(vid))
        .any(|ty| type_contains_i128(module, &ty));

    match term {
        Terminator::Return(Some(val)) => func
            .value_type(*val)
            .is_none_or(|ty| abi_type_i128_backend_o0_supported(module, &ty, true)),
        Terminator::Branch(_, args) => args
            .iter()
            .filter_map(|arg| func.value_type(*arg))
            .all(|ty| ssa_type_i128_backend_o0_supported(module, &ty)),
        Terminator::CondBranch {
            true_args,
            false_args,
            ..
        } => true_args
            .iter()
            .chain(false_args.iter())
            .filter_map(|arg| func.value_type(*arg))
            .all(|ty| ssa_type_i128_backend_o0_supported(module, &ty)),
        _ => !term_uses_i128,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::builder::FuncBuilder;

    #[test]
    fn runtime_print_i128_is_supported_by_backend_gate() {
        let mut module = Module::new("test".into());
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let wide = b.const_i128(170141183460469231731687303715884105727i128);
            b.runtime_call(RuntimeFunc::PrintInt, vec![wide], IrType::Void);
            b.ret_void();
        }
        module.add_function(func);

        assert!(
            module.i128_backend_o0_supported(),
            "runtime PrintInt with integer(16) should stay inside the supported O0 backend surface"
        );
    }

    #[test]
    fn byte_cursor_gep_into_i128_storage_stays_supported() {
        let mut module = Module::new("test".into());
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let arr = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I128)), 3));
            let off = b.const_i64(16);
            let cursor = b.gep(arr, vec![off], IrType::Int(IntWidth::I8));
            let wide = b.const_i128(170141183460469231731687303715884105727i128);
            b.store(wide, cursor);
            b.ret_void();
        }
        module.add_function(func);

        assert!(
            module.i128_backend_o0_supported(),
            "byte-cursor GEPs into i128 storage should stay inside the supported backend surface"
        );
    }
}
