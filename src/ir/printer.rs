//! IR textual printer — human-readable representation for debugging.
//!
//! Used by `-emit-ir` and in test assertions.

use std::fmt::Write;
use super::inst::*;

/// Print a module to a string.
pub fn print_module(module: &Module) -> String {
    let mut out = String::new();
    writeln!(out, "module {}", module.name).unwrap();

    for sd in &module.struct_defs {
        write!(out, "  struct {} {{ ", sd.name).unwrap();
        for (i, (name, ty)) in sd.fields.iter().enumerate() {
            if i > 0 { write!(out, ", ").unwrap(); }
            write!(out, "{}: {}", name, ty).unwrap();
        }
        writeln!(out, " }}").unwrap();
    }

    for g in &module.globals {
        write!(out, "  global @{}: {}", g.name, g.ty).unwrap();
        if let Some(init) = &g.initializer {
            match init {
                GlobalInit::Zero => write!(out, " = zeroinit").unwrap(),
                GlobalInit::Int(v) => write!(out, " = {}", v).unwrap(),
                GlobalInit::Float(v) => write!(out, " = {}", v).unwrap(),
                GlobalInit::String(s) => write!(out, " = {:?}", String::from_utf8_lossy(s)).unwrap(),
                GlobalInit::IntArray(vs) => {
                    let s: Vec<String> = vs.iter().map(|v| v.to_string()).collect();
                    write!(out, " = [{}]", s.join(", ")).unwrap();
                }
                GlobalInit::FloatArray(vs) => {
                    let s: Vec<String> = vs.iter().map(|v| v.to_string()).collect();
                    write!(out, " = [{}]", s.join(", ")).unwrap();
                }
            }
        }
        writeln!(out).unwrap();
    }

    for ef in &module.extern_funcs {
        write!(out, "  extern fn @{}(", ef.name).unwrap();
        for (i, p) in ef.sig.params.iter().enumerate() {
            if i > 0 { write!(out, ", ").unwrap(); }
            write!(out, "{}", p).unwrap();
        }
        writeln!(out, ") -> {}", ef.sig.ret).unwrap();
    }

    for func in &module.functions {
        writeln!(out).unwrap();
        write!(out, "{}", print_function_in(module, func)).unwrap();
    }

    out
}

fn print_function_in(module: &Module, func: &Function) -> String {
    let mut out = String::new();
    // Print function attributes.
    let mut attrs = Vec::new();
    if func.is_pure { attrs.push("pure"); }
    if func.is_elemental { attrs.push("elemental"); }
    let attr_str = if attrs.is_empty() { String::new() } else { format!(" [{}]", attrs.join(", ")) };
    write!(out, "  func @{}{}(", func.name, attr_str).unwrap();
    for (i, p) in func.params.iter().enumerate() {
        if i > 0 { write!(out, ", ").unwrap(); }
        write!(out, "%{}: {}", p.id.0, p.ty).unwrap();
    }
    writeln!(out, ") -> {} {{", func.return_type).unwrap();

    for block in &func.blocks {
        write!(out, "{}", print_block_with_module(block, func, module)).unwrap();
    }

    writeln!(out, "  }}").unwrap();
    out
}

/// Print a function to a string.
pub fn print_function(func: &Function) -> String {
    print_function_with_module_opt(None, func)
}

/// Print a basic block to a string (with function context for named branches).
pub fn print_block_in(block: &BasicBlock, func: &Function) -> String {
    print_block_with_module_opt(block, func, None)
}

fn print_block_with_module(block: &BasicBlock, func: &Function, module: &Module) -> String {
    print_block_with_module_opt(block, func, Some(module))
}

fn print_block_with_module_opt(block: &BasicBlock, func: &Function, module: Option<&Module>) -> String {
    let mut out = String::new();
    write!(out, "    {}(", block.name).unwrap();
    for (i, bp) in block.params.iter().enumerate() {
        if i > 0 { write!(out, ", ").unwrap(); }
        write!(out, "%{}: {}", bp.id.0, bp.ty).unwrap();
    }
    writeln!(out, "):").unwrap();

    for inst in &block.insts {
        writeln!(out, "      {}", print_inst_with_module_opt(inst, module)).unwrap();
    }

    if let Some(term) = &block.terminator {
        writeln!(out, "      {}", print_terminator_with_names(term, func)).unwrap();
    }

    out
}

/// Print a basic block to a string (fallback, no function context).
pub fn print_block(block: &BasicBlock) -> String {
    let mut out = String::new();
    write!(out, "    {}(", block.name).unwrap();
    for (i, bp) in block.params.iter().enumerate() {
        if i > 0 { write!(out, ", ").unwrap(); }
        write!(out, "%{}: {}", bp.id.0, bp.ty).unwrap();
    }
    writeln!(out, "):").unwrap();

    for inst in &block.insts {
        writeln!(out, "      {}", print_inst(inst)).unwrap();
    }

    if let Some(term) = &block.terminator {
        writeln!(out, "      {}", print_terminator(term)).unwrap();
    }

    out
}

/// Print an instruction to a string.
pub fn print_inst(inst: &Inst) -> String {
    print_inst_with_module_opt(inst, None)
}

fn print_function_with_module_opt(module: Option<&Module>, func: &Function) -> String {
    let mut out = String::new();
    // Print function attributes.
    let mut attrs = Vec::new();
    if func.is_pure { attrs.push("pure"); }
    if func.is_elemental { attrs.push("elemental"); }
    let attr_str = if attrs.is_empty() { String::new() } else { format!(" [{}]", attrs.join(", ")) };
    write!(out, "  func @{}{}(", func.name, attr_str).unwrap();
    for (i, p) in func.params.iter().enumerate() {
        if i > 0 { write!(out, ", ").unwrap(); }
        write!(out, "%{}: {}", p.id.0, p.ty).unwrap();
    }
    writeln!(out, ") -> {} {{", func.return_type).unwrap();

    for block in &func.blocks {
        write!(out, "{}", print_block_with_module_opt(block, func, module)).unwrap();
    }

    writeln!(out, "  }}").unwrap();
    out
}

fn print_inst_with_module_opt(inst: &Inst, module: Option<&Module>) -> String {
    let val = format!("%{}", inst.id.0);
    let kind_str = match &inst.kind {
        InstKind::ConstInt(v, w) => format!("const_int {} : {}", v, w),
        InstKind::ConstFloat(v, w) => format!("const_float {} : {}", v, w),
        InstKind::ConstBool(v) => format!("const_bool {}", v),
        InstKind::ConstString(s) => format!("const_string {:?}", String::from_utf8_lossy(s)),
        InstKind::Undef(ty) => format!("undef : {}", ty),

        InstKind::IAdd(a, b) => format!("iadd %{}, %{}", a.0, b.0),
        InstKind::ISub(a, b) => format!("isub %{}, %{}", a.0, b.0),
        InstKind::IMul(a, b) => format!("imul %{}, %{}", a.0, b.0),
        InstKind::IDiv(a, b) => format!("idiv %{}, %{}", a.0, b.0),
        InstKind::IMod(a, b) => format!("imod %{}, %{}", a.0, b.0),
        InstKind::INeg(a) => format!("ineg %{}", a.0),

        InstKind::FAdd(a, b) => format!("fadd %{}, %{}", a.0, b.0),
        InstKind::FSub(a, b) => format!("fsub %{}, %{}", a.0, b.0),
        InstKind::FMul(a, b) => format!("fmul %{}, %{}", a.0, b.0),
        InstKind::FDiv(a, b) => format!("fdiv %{}, %{}", a.0, b.0),
        InstKind::FNeg(a) => format!("fneg %{}", a.0),
        InstKind::FAbs(a) => format!("fabs %{}", a.0),
        InstKind::FSqrt(a) => format!("fsqrt %{}", a.0),
        InstKind::FPow(a, b) => format!("fpow %{}, %{}", a.0, b.0),

        InstKind::ICmp(op, a, b) => format!("icmp {} %{}, %{}", cmp_str(*op), a.0, b.0),
        InstKind::FCmp(op, a, b) => format!("fcmp {} %{}, %{}", cmp_str(*op), a.0, b.0),

        InstKind::And(a, b) => format!("and %{}, %{}", a.0, b.0),
        InstKind::Or(a, b) => format!("or %{}, %{}", a.0, b.0),
        InstKind::Not(a) => format!("not %{}", a.0),

        InstKind::Select(c, t, f) => format!("select %{}, %{}, %{}", c.0, t.0, f.0),

        InstKind::BitAnd(a, b) => format!("bitand %{}, %{}", a.0, b.0),
        InstKind::BitOr(a, b) => format!("bitor %{}, %{}", a.0, b.0),
        InstKind::BitXor(a, b) => format!("bitxor %{}, %{}", a.0, b.0),
        InstKind::BitNot(a) => format!("bitnot %{}", a.0),
        InstKind::Shl(a, b) => format!("shl %{}, %{}", a.0, b.0),
        InstKind::LShr(a, b) => format!("lshr %{}, %{}", a.0, b.0),
        InstKind::AShr(a, b) => format!("ashr %{}, %{}", a.0, b.0),
        InstKind::CountLeadingZeros(a) => format!("clz %{}", a.0),
        InstKind::CountTrailingZeros(a) => format!("ctz %{}", a.0),
        InstKind::PopCount(a) => format!("popcount %{}", a.0),

        InstKind::IntToFloat(v, w) => format!("int_to_float %{} : {}", v.0, w),
        InstKind::FloatToInt(v, w) => format!("float_to_int %{} : {}", v.0, w),
        InstKind::FloatExtend(v, w) => format!("float_extend %{} : {}", v.0, w),
        InstKind::FloatTrunc(v, w) => format!("float_trunc %{} : {}", v.0, w),
        InstKind::IntExtend(v, w, s) => format!("int_extend %{} : {} {}", v.0, w, if *s { "signed" } else { "unsigned" }),
        InstKind::IntTrunc(v, w) => format!("int_trunc %{} : {}", v.0, w),
        InstKind::PtrToInt(v) => format!("ptr_to_int %{}", v.0),
        InstKind::IntToPtr(v, ty) => format!("int_to_ptr %{} : {}", v.0, ty),

        InstKind::Alloca(ty) => format!("alloca {}", ty),
        InstKind::Load(a) => format!("load %{}", a.0),
        InstKind::Store(v, a) => format!("store %{}, %{}", v.0, a.0),
        InstKind::GetElementPtr(base, idxs) => {
            let idx_str: Vec<String> = idxs.iter().map(|i| format!("%{}", i.0)).collect();
            format!("gep %{}, [{}]", base.0, idx_str.join(", "))
        }
        InstKind::GlobalAddr(name) => format!("global_addr @{}", name),

        InstKind::Call(fref, args) => {
            let args_str: Vec<String> = args.iter().map(|a| format!("%{}", a.0)).collect();
            let fname = match fref {
                FuncRef::Internal(idx) => module
                    .and_then(|module| module.functions.get(*idx as usize))
                    .map(|func| format!("@{}", func.name))
                    .unwrap_or_else(|| format!("@func_{}", idx)),
                FuncRef::External(name) => format!("@{}", name),
                FuncRef::Indirect(target) => format!("%{}", target.0),
            };
            format!("call {}({})", fname, args_str.join(", "))
        }
        InstKind::RuntimeCall(rf, args) => {
            let args_str: Vec<String> = args.iter().map(|a| format!("%{}", a.0)).collect();
            format!("rt_call @{}({})", runtime_func_name(rf), args_str.join(", "))
        }

        InstKind::ExtractField(agg, idx) => format!("extract_field %{}, {}", agg.0, idx),
        InstKind::InsertField(agg, idx, val) => format!("insert_field %{}, {}, %{}", agg.0, idx, val.0),
    };

    if inst.ty == super::types::IrType::Void {
        kind_str
    } else {
        format!("{} = {} : {}", val, kind_str, inst.ty)
    }
}

/// Print a terminator to a string. Uses block names for readability.
pub fn print_terminator_with_names(term: &Terminator, func: &Function) -> String {
    let bname = |id: &BlockId| -> String { func.block(*id).name.clone() };
    match term {
        Terminator::Return(None) => "ret void".into(),
        Terminator::Return(Some(v)) => format!("ret %{}", v.0),
        Terminator::Branch(dest, args) => {
            if args.is_empty() {
                format!("br {}", bname(dest))
            } else {
                let args_str: Vec<String> = args.iter().map(|a| format!("%{}", a.0)).collect();
                format!("br {}({})", bname(dest), args_str.join(", "))
            }
        }
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } => {
            let ta: Vec<String> = true_args.iter().map(|a| format!("%{}", a.0)).collect();
            let fa: Vec<String> = false_args.iter().map(|a| format!("%{}", a.0)).collect();
            format!("cond_br %{}, {}({}), {}({})",
                cond.0, bname(true_dest), ta.join(", "), bname(false_dest), fa.join(", "))
        }
        Terminator::Switch { selector, cases, default } => {
            let cases_str: Vec<String> = cases.iter()
                .map(|(v, b)| format!("{} -> {}", v, bname(b)))
                .collect();
            format!("switch %{}, [{}], default {}", selector.0, cases_str.join(", "), bname(default))
        }
        Terminator::Unreachable => "unreachable".into(),
    }
}

/// Print a terminator without function context (fallback to block IDs).
pub fn print_terminator(term: &Terminator) -> String {
    match term {
        Terminator::Return(None) => "ret void".into(),
        Terminator::Return(Some(v)) => format!("ret %{}", v.0),
        Terminator::Branch(dest, args) => {
            if args.is_empty() {
                format!("br bb{}", dest.0)
            } else {
                let args_str: Vec<String> = args.iter().map(|a| format!("%{}", a.0)).collect();
                format!("br bb{}({})", dest.0, args_str.join(", "))
            }
        }
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } => {
            let ta: Vec<String> = true_args.iter().map(|a| format!("%{}", a.0)).collect();
            let fa: Vec<String> = false_args.iter().map(|a| format!("%{}", a.0)).collect();
            format!("cond_br %{}, bb{}({}), bb{}({})",
                cond.0, true_dest.0, ta.join(", "), false_dest.0, fa.join(", "))
        }
        Terminator::Switch { selector, cases, default } => {
            let cases_str: Vec<String> = cases.iter()
                .map(|(v, b)| format!("{} -> bb{}", v, b.0))
                .collect();
            format!("switch %{}, [{}], default bb{}", selector.0, cases_str.join(", "), default.0)
        }
        Terminator::Unreachable => "unreachable".into(),
    }
}

fn cmp_str(op: super::inst::CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "eq",
        CmpOp::Ne => "ne",
        CmpOp::Lt => "lt",
        CmpOp::Le => "le",
        CmpOp::Gt => "gt",
        CmpOp::Ge => "ge",
    }
}

fn runtime_func_name(rf: &RuntimeFunc) -> &'static str {
    match rf {
        RuntimeFunc::PrintInt => "__afs_print_int",
        RuntimeFunc::PrintReal => "__afs_print_real",
        RuntimeFunc::PrintString => "__afs_print_string",
        RuntimeFunc::PrintLogical => "__afs_print_logical",
        RuntimeFunc::PrintNewline => "__afs_print_newline",
        RuntimeFunc::Allocate => "__afs_allocate",
        RuntimeFunc::Deallocate => "__afs_deallocate",
        RuntimeFunc::StringConcat => "__afs_string_concat",
        RuntimeFunc::StringCopy => "__afs_string_copy",
        RuntimeFunc::StringCompare => "__afs_string_compare",
        RuntimeFunc::Stop => "__afs_stop",
        RuntimeFunc::ErrorStop => "__afs_error_stop",
        RuntimeFunc::CheckBounds => "__afs_check_bounds",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::*;
    use super::super::builder::FuncBuilder;

    #[test]
    fn print_simple_function() {
        let mut module = Module::new("test".into());
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let z = b.iadd(x, y);
            b.runtime_call(RuntimeFunc::PrintInt, vec![z], IrType::Void);
            b.ret_void();
        }
        module.add_function(func);

        let output = print_module(&module);
        assert!(output.contains("module test"));
        assert!(output.contains("func @main()"));
        assert!(output.contains("const_int 10"));
        assert!(output.contains("const_int 20"));
        assert!(output.contains("iadd"));
        assert!(output.contains("rt_call @__afs_print_int"));
        assert!(output.contains("ret void"));
    }

    #[test]
    fn print_branch() {
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
        let output = print_function(&func);
        assert!(output.contains("cond_br %0, then_1(), else_2()"));
    }

    #[test]
    fn print_alloca_load_store() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            let val = b.const_i32(42);
            b.store(val, addr);
            let _ = b.load(addr);
            b.ret_void();
        }
        let output = print_function(&func);
        assert!(output.contains("alloca i32"));
        assert!(output.contains("store"));
        assert!(output.contains("load"));
    }

    #[test]
    fn print_block_params() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let _p = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let init = b.const_i32(0);
            b.branch(header, vec![init]);

            b.set_block(header);
            b.ret_void();
        }
        let output = print_function(&func);
        assert!(output.contains("header_1(%"));
        assert!(output.contains(": i32)"));
        assert!(output.contains("br header_1("), "expected 'br header_1(' in:\n{}", output);
    }

    #[test]
    fn print_internal_calls_with_function_names() {
        let mut module = Module::new("test".into());
        let callee_idx = module.add_function(Function::new(
            "callee".into(),
            vec![Param {
                name: "x".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            }],
            IrType::Int(IntWidth::I32),
        ));

        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut caller);
            let arg = b.const_i32(7);
            let _ = b.call(FuncRef::Internal(callee_idx), vec![arg], IrType::Int(IntWidth::I32));
            b.ret_void();
        }
        module.add_function(caller);

        let output = print_module(&module);
        assert!(output.contains("call @callee("), "expected named internal call in:\n{}", output);
        assert!(!output.contains("@func_0"), "unexpected fallback name in:\n{}", output);
    }
}
