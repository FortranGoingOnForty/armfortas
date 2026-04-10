//! IR instructions, terminators, and values.
//!
//! Every instruction produces a value (ValueId) in SSA form.
//! Basic blocks end with exactly one Terminator.

use super::types::{IrType, IntWidth, FloatWidth};
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
    IntExtend(ValueId, IntWidth, bool),   // bool = signed
    IntTrunc(ValueId, IntWidth),

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
        self.blocks.iter().find(|b| b.id == id).expect("block not found")
    }

    /// Get a mutable block by ID. Panics if the ID is not present.
    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        self.blocks.iter_mut().find(|b| b.id == id).expect("block not found")
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
        self.type_cache.get(&id).cloned()
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
        self.globals.iter().any(|global| type_contains_i128(self, &global.ty))
            || self.extern_funcs.iter().any(|func| sig_contains_i128(self, &func.sig))
            || self.functions.iter().any(|func| {
                type_contains_i128(self, &func.return_type)
                    || func.params.iter().any(|param| type_contains_i128(self, &param.ty))
                    || func.blocks.iter().any(|block| {
                        block.params.iter().any(|param| type_contains_i128(self, &param.ty))
                            || block.insts.iter().any(|inst| type_contains_i128(self, &inst.ty))
                    })
            })
    }
}

fn sig_contains_i128(module: &Module, sig: &super::types::FuncSig) -> bool {
    type_contains_i128(module, &sig.ret)
        || sig.params.iter().any(|param| type_contains_i128(module, param))
}

fn type_contains_i128(module: &Module, ty: &IrType) -> bool {
    match ty {
        IrType::Int(IntWidth::I128) => true,
        IrType::Ptr(inner) | IrType::Array(inner, _) => type_contains_i128(module, inner),
        IrType::Struct(id) => module
            .struct_defs
            .get(*id as usize)
            .is_some_and(|def| def.fields.iter().any(|(_, field_ty)| type_contains_i128(module, field_ty))),
        IrType::FuncPtr(sig) => sig_contains_i128(module, sig),
        _ => false,
    }
}
