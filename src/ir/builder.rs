//! IR builder — ergonomic API for constructing IR programmatically.
//!
//! Used by the AST→IR lowering pass and by tests. Manages ValueId
//! allocation, block insertion, and instruction emission.

use super::inst::*;
use super::types::*;
use crate::lexer::{Position, Span};

/// Builder for constructing a function's IR.
pub struct FuncBuilder<'a> {
    func: &'a mut Function,
    current_block: BlockId,
}

impl<'a> FuncBuilder<'a> {
    pub fn new(func: &'a mut Function) -> Self {
        let entry = func.entry;
        Self {
            func,
            current_block: entry,
        }
    }

    /// Switch to emitting into a different block.
    pub fn set_block(&mut self, block: BlockId) {
        self.current_block = block;
    }

    /// Get the current block ID.
    pub fn current_block(&self) -> BlockId {
        self.current_block
    }

    /// Create a new basic block and return its ID.
    pub fn create_block(&mut self, name: &str) -> BlockId {
        self.func.create_block(name)
    }

    /// Add a block parameter and return its ValueId.
    pub fn add_block_param(&mut self, block: BlockId, ty: IrType) -> ValueId {
        let id = self.func.next_value_id();
        self.func.register_type(id, ty.clone());
        self.func
            .block_mut(block)
            .params
            .push(BlockParam { id, ty });
        id
    }

    /// Get the underlying function.
    pub fn func(&self) -> &Function {
        self.func
    }

    // ---- Instruction emission ----

    fn emit(&mut self, kind: InstKind, ty: IrType) -> ValueId {
        let id = self.func.next_value_id();
        self.func.register_type(id, ty.clone());
        let inst = Inst {
            id,
            kind,
            ty,
            span: dummy_span(),
        };
        self.func.block_mut(self.current_block).insts.push(inst);
        id
    }

    fn emit_with_span(&mut self, kind: InstKind, ty: IrType, span: Span) -> ValueId {
        let id = self.func.next_value_id();
        self.func.register_type(id, ty.clone());
        let inst = Inst { id, kind, ty, span };
        self.func.block_mut(self.current_block).insts.push(inst);
        id
    }

    // ---- Constants ----

    pub fn const_int(&mut self, value: i128, width: IntWidth) -> ValueId {
        self.emit(InstKind::ConstInt(value, width), IrType::Int(width))
    }

    pub fn const_i32(&mut self, value: i32) -> ValueId {
        self.const_int(value as i128, IntWidth::I32)
    }

    pub fn const_i64(&mut self, value: i64) -> ValueId {
        self.const_int(value as i128, IntWidth::I64)
    }

    pub fn const_i128(&mut self, value: i128) -> ValueId {
        self.const_int(value, IntWidth::I128)
    }

    pub fn const_float(&mut self, value: f64, width: FloatWidth) -> ValueId {
        self.emit(InstKind::ConstFloat(value, width), IrType::Float(width))
    }

    pub fn const_f32(&mut self, value: f32) -> ValueId {
        self.const_float(value as f64, FloatWidth::F32)
    }

    pub fn const_f64(&mut self, value: f64) -> ValueId {
        self.const_float(value, FloatWidth::F64)
    }

    pub fn const_bool(&mut self, value: bool) -> ValueId {
        self.emit(InstKind::ConstBool(value), IrType::Bool)
    }

    pub fn const_string(&mut self, value: &[u8]) -> ValueId {
        let ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));
        self.emit(InstKind::ConstString(value.to_vec()), ty)
    }

    pub fn undef(&mut self, ty: IrType) -> ValueId {
        let t = ty.clone();
        self.emit(InstKind::Undef(ty), t)
    }

    // ---- Integer arithmetic ----

    pub fn iadd(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::IAdd(lhs, rhs), ty)
    }

    pub fn isub(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::ISub(lhs, rhs), ty)
    }

    pub fn imul(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::IMul(lhs, rhs), ty)
    }

    pub fn idiv(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::IDiv(lhs, rhs), ty)
    }

    pub fn imod(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::IMod(lhs, rhs), ty)
    }

    pub fn ineg(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::INeg(val), ty)
    }

    // ---- Float arithmetic ----

    pub fn fadd(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Float(FloatWidth::F32));
        self.emit(InstKind::FAdd(lhs, rhs), ty)
    }

    pub fn fsub(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Float(FloatWidth::F32));
        self.emit(InstKind::FSub(lhs, rhs), ty)
    }

    pub fn fmul(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Float(FloatWidth::F32));
        self.emit(InstKind::FMul(lhs, rhs), ty)
    }

    pub fn fdiv(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Float(FloatWidth::F32));
        self.emit(InstKind::FDiv(lhs, rhs), ty)
    }

    pub fn fneg(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Float(FloatWidth::F32));
        self.emit(InstKind::FNeg(val), ty)
    }

    pub fn fabs(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Float(FloatWidth::F64));
        self.emit(InstKind::FAbs(val), ty)
    }

    pub fn fsqrt(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Float(FloatWidth::F64));
        self.emit(InstKind::FSqrt(val), ty)
    }

    pub fn fpow(&mut self, base: ValueId, exp: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(base)
            .unwrap_or(IrType::Float(FloatWidth::F64));
        self.emit(InstKind::FPow(base, exp), ty)
    }

    // ---- Comparison ----

    pub fn icmp(&mut self, op: CmpOp, lhs: ValueId, rhs: ValueId) -> ValueId {
        self.emit(InstKind::ICmp(op, lhs, rhs), IrType::Bool)
    }

    pub fn fcmp(&mut self, op: CmpOp, lhs: ValueId, rhs: ValueId) -> ValueId {
        self.emit(InstKind::FCmp(op, lhs, rhs), IrType::Bool)
    }

    // ---- Select ----

    /// Conditional select: cond ? true_val : false_val
    pub fn select(&mut self, cond: ValueId, true_val: ValueId, false_val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(true_val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::Select(cond, true_val, false_val), ty)
    }

    // ---- Logic ----

    pub fn and(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        self.emit(InstKind::And(lhs, rhs), IrType::Bool)
    }

    pub fn or(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        self.emit(InstKind::Or(lhs, rhs), IrType::Bool)
    }

    pub fn not(&mut self, val: ValueId) -> ValueId {
        self.emit(InstKind::Not(val), IrType::Bool)
    }

    // ---- Bitwise ----

    pub fn bit_and(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::BitAnd(lhs, rhs), ty)
    }

    pub fn bit_or(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::BitOr(lhs, rhs), ty)
    }

    pub fn bit_xor(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::BitXor(lhs, rhs), ty)
    }

    pub fn shl(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::Shl(lhs, rhs), ty)
    }

    pub fn bit_not(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::BitNot(val), ty)
    }

    pub fn lshr(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(lhs)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::LShr(lhs, rhs), ty)
    }

    pub fn clz(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::CountLeadingZeros(val), ty)
    }

    pub fn ctz(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::CountTrailingZeros(val), ty)
    }

    pub fn popcount(&mut self, val: ValueId) -> ValueId {
        let ty = self
            .func
            .value_type(val)
            .unwrap_or(IrType::Int(IntWidth::I32));
        self.emit(InstKind::PopCount(val), ty)
    }

    // ---- Conversions ----

    pub fn int_to_float(&mut self, val: ValueId, width: FloatWidth) -> ValueId {
        self.emit(InstKind::IntToFloat(val, width), IrType::Float(width))
    }

    pub fn float_to_int(&mut self, val: ValueId, width: IntWidth) -> ValueId {
        self.emit(InstKind::FloatToInt(val, width), IrType::Int(width))
    }

    pub fn float_extend(&mut self, val: ValueId, width: FloatWidth) -> ValueId {
        self.emit(InstKind::FloatExtend(val, width), IrType::Float(width))
    }

    pub fn float_trunc(&mut self, val: ValueId, width: FloatWidth) -> ValueId {
        self.emit(InstKind::FloatTrunc(val, width), IrType::Float(width))
    }

    pub fn int_extend(&mut self, val: ValueId, width: IntWidth, signed: bool) -> ValueId {
        self.emit(InstKind::IntExtend(val, width, signed), IrType::Int(width))
    }

    pub fn int_trunc(&mut self, val: ValueId, width: IntWidth) -> ValueId {
        self.emit(InstKind::IntTrunc(val, width), IrType::Int(width))
    }

    pub fn ptr_to_int(&mut self, val: ValueId) -> ValueId {
        self.emit(InstKind::PtrToInt(val), IrType::Int(IntWidth::I64))
    }

    pub fn int_to_ptr(&mut self, val: ValueId, pointee: IrType) -> ValueId {
        let ptr_ty = IrType::Ptr(Box::new(pointee));
        self.emit(InstKind::IntToPtr(val, ptr_ty.clone()), ptr_ty)
    }

    // ---- Memory ----

    pub fn alloca(&mut self, ty: IrType) -> ValueId {
        let ptr_ty = IrType::Ptr(Box::new(ty.clone()));
        self.emit(InstKind::Alloca(ty), ptr_ty)
    }

    /// Take the address of a module-level global. The result is
    /// `Ptr<elem_ty>` and can flow into Load/Store/GEP just like
    /// any other pointer. The global itself must be added to the
    /// containing Module by the caller (typically lower.rs).
    pub fn global_addr(&mut self, name: &str, elem_ty: IrType) -> ValueId {
        let ptr_ty = IrType::Ptr(Box::new(elem_ty));
        self.emit(InstKind::GlobalAddr(name.into()), ptr_ty)
    }

    pub fn load(&mut self, addr: ValueId) -> ValueId {
        let ty = match self.func.value_type(addr) {
            Some(IrType::Ptr(inner)) => *inner,
            _ => IrType::Int(IntWidth::I64), // fallback
        };
        self.emit(InstKind::Load(addr), ty)
    }

    /// Load with an explicit result type (ignoring the pointer's inner type).
    /// Used for loading fields from aggregate pointers (e.g., first 8 bytes of a descriptor).
    pub fn load_typed(&mut self, addr: ValueId, ty: IrType) -> ValueId {
        self.emit(InstKind::Load(addr), ty)
    }

    pub fn store(&mut self, value: ValueId, addr: ValueId) -> ValueId {
        self.emit(InstKind::Store(value, addr), IrType::Void)
    }

    pub fn gep(&mut self, base: ValueId, indices: Vec<ValueId>, result_ty: IrType) -> ValueId {
        self.emit(
            InstKind::GetElementPtr(base, indices),
            IrType::Ptr(Box::new(result_ty)),
        )
    }

    // ---- Calls ----

    pub fn call(&mut self, func_ref: FuncRef, args: Vec<ValueId>, ret_ty: IrType) -> ValueId {
        self.emit(InstKind::Call(func_ref, args), ret_ty)
    }

    pub fn runtime_call(
        &mut self,
        func: RuntimeFunc,
        args: Vec<ValueId>,
        ret_ty: IrType,
    ) -> ValueId {
        self.emit(InstKind::RuntimeCall(func, args), ret_ty)
    }

    // ---- Aggregates ----

    pub fn extract_field(&mut self, agg: ValueId, field: u32, field_ty: IrType) -> ValueId {
        self.emit(InstKind::ExtractField(agg, field), field_ty)
    }

    pub fn insert_field(&mut self, agg: ValueId, field: u32, value: ValueId) -> ValueId {
        let ty = self.func.value_type(agg).unwrap_or(IrType::Void);
        self.emit(InstKind::InsertField(agg, field, value), ty)
    }

    // ---- Terminators ----

    pub fn ret(&mut self, value: Option<ValueId>) {
        self.func.block_mut(self.current_block).terminator = Some(Terminator::Return(value));
    }

    pub fn ret_void(&mut self) {
        self.ret(None);
    }

    pub fn branch(&mut self, dest: BlockId, args: Vec<ValueId>) {
        self.func.block_mut(self.current_block).terminator = Some(Terminator::Branch(dest, args));
    }

    pub fn cond_branch(
        &mut self,
        cond: ValueId,
        true_dest: BlockId,
        true_args: Vec<ValueId>,
        false_dest: BlockId,
        false_args: Vec<ValueId>,
    ) {
        self.func.block_mut(self.current_block).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        });
    }

    pub fn switch(&mut self, selector: ValueId, cases: Vec<(i64, BlockId)>, default: BlockId) {
        self.func.block_mut(self.current_block).terminator = Some(Terminator::Switch {
            selector,
            cases,
            default,
        });
    }

    pub fn unreachable(&mut self) {
        self.func.block_mut(self.current_block).terminator = Some(Terminator::Unreachable);
    }

    // ---- Test helpers (bypass type inference for verifier tests) ----
    #[cfg(test)]
    pub fn emit_bogus_iadd(&mut self, a: ValueId, b: ValueId) -> ValueId {
        self.emit(InstKind::IAdd(a, b), IrType::Int(IntWidth::I32))
    }

    #[cfg(test)]
    pub fn emit_bogus_store(&mut self, val: ValueId, addr: ValueId) -> ValueId {
        self.emit(InstKind::Store(val, addr), IrType::Void)
    }
}

fn dummy_span() -> Span {
    Span {
        file_id: 0,
        start: Position { line: 0, col: 0 },
        end: Position { line: 0, col: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_simple_function() {
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let z = b.iadd(x, y);
            b.runtime_call(RuntimeFunc::PrintInt, vec![z], IrType::Void);
            b.ret_void();
        }
        assert_eq!(func.blocks.len(), 1);
        assert_eq!(func.blocks[0].insts.len(), 4); // const, const, iadd, runtime_call
        assert!(func.blocks[0].terminator.is_some());
    }

    #[test]
    fn build_branching_function() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let cond = b.const_bool(true);
            let bb_true = b.create_block("then");
            let bb_false = b.create_block("else");
            b.cond_branch(cond, bb_true, vec![], bb_false, vec![]);

            b.set_block(bb_true);
            b.ret_void();

            b.set_block(bb_false);
            b.ret_void();
        }
        assert_eq!(func.blocks.len(), 3);
    }

    #[test]
    fn build_block_params() {
        let mut func = Function::new("loop".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let i_param = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let init = b.const_i32(0);
            b.branch(header, vec![init]);

            b.set_block(header);
            let one = b.const_i32(1);
            let next = b.iadd(i_param, one);
            let limit = b.const_i32(10);
            let done = b.icmp(CmpOp::Ge, next, limit);
            let exit = b.create_block("exit");
            b.cond_branch(done, exit, vec![], header, vec![next]);

            b.set_block(exit);
            b.ret_void();
        }
        assert_eq!(func.blocks.len(), 3);
        assert_eq!(func.block(BlockId(1)).params.len(), 1); // header has 1 param
    }

    #[test]
    fn alloca_load_store() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            let val = b.const_i32(42);
            b.store(val, addr);
            let loaded = b.load(addr);
            b.runtime_call(RuntimeFunc::PrintInt, vec![loaded], IrType::Void);
            b.ret_void();
        }
        assert_eq!(func.blocks[0].insts.len(), 5); // alloca, const, store, load, runtime_call
    }

    #[test]
    fn float_arithmetic() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let a = b.const_f64(3.14);
            let c = b.const_f64(2.0);
            let sum = b.fadd(a, c);
            let neg = b.fneg(sum);
            b.runtime_call(RuntimeFunc::PrintReal, vec![neg], IrType::Void);
            b.ret_void();
        }
        assert_eq!(func.blocks[0].insts.len(), 5);
    }

    #[test]
    fn conversions() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let i = b.const_i32(42);
            let f = b.int_to_float(i, FloatWidth::F64);
            let back = b.float_to_int(f, IntWidth::I32);
            let _ = back;
            b.ret_void();
        }
        assert_eq!(func.blocks[0].insts.len(), 3);
    }
}
