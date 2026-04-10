//! Bench-facing capture helpers.
//!
//! This module exposes a stable-ish API for collecting intermediate artifacts
//! from the compiler pipeline so the external `afs-tests` runner can assert on
//! more than just final program output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::codegen::mir::{
    ArmCond, ConstPoolEntry, MachineFunction, MachineInst, MachineOperand, MBlockId, PhysReg,
    RegClass,
};
use crate::codegen::{emit, isel, linearscan};
use crate::driver::OptLevel;
use crate::opt::build_pipeline;
use crate::opt::pipeline::OptLevel as IrOptLevel;
use crate::ir::{lower, printer as ir_printer, verify};
use crate::lexer::{detect_source_form, tokenize, SourceForm, Token};
use crate::parser::Parser;
use crate::sema::{resolve, validate};

static CAPTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Capturable compiler stages for the bench.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Preprocess,
    Tokens,
    Ast,
    Sema,
    Ir,
    OptIr,
    Mir,
    Regalloc,
    Asm,
    Obj,
    Run,
}

impl Stage {
    pub const ALL: [Stage; 11] = [
        Stage::Preprocess,
        Stage::Tokens,
        Stage::Ast,
        Stage::Sema,
        Stage::Ir,
        Stage::OptIr,
        Stage::Mir,
        Stage::Regalloc,
        Stage::Asm,
        Stage::Obj,
        Stage::Run,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "preprocess" => Some(Self::Preprocess),
            "tokens" => Some(Self::Tokens),
            "ast" => Some(Self::Ast),
            "sema" => Some(Self::Sema),
            "ir" => Some(Self::Ir),
            "optir" => Some(Self::OptIr),
            "mir" => Some(Self::Mir),
            "regalloc" => Some(Self::Regalloc),
            "asm" => Some(Self::Asm),
            "obj" => Some(Self::Obj),
            "run" => Some(Self::Run),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preprocess => "preprocess",
            Self::Tokens => "tokens",
            Self::Ast => "ast",
            Self::Sema => "sema",
            Self::Ir => "ir",
            Self::OptIr => "optir",
            Self::Mir => "mir",
            Self::Regalloc => "regalloc",
            Self::Asm => "asm",
            Self::Obj => "obj",
            Self::Run => "run",
        }
    }
}

/// Failure point while trying to capture compiler stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureStage {
    Preprocess,
    Lexer,
    Parser,
    Sema,
    Ir,
    Obj,
    Run,
}

impl FailureStage {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "preprocess" => Some(Self::Preprocess),
            "lexer" | "tokens" => Some(Self::Lexer),
            "parser" | "parse" | "ast" => Some(Self::Parser),
            "sema" => Some(Self::Sema),
            "ir" | "optir" => Some(Self::Ir),
            "obj" | "asm" => Some(Self::Obj),
            "run" => Some(Self::Run),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preprocess => "preprocess",
            Self::Lexer => "lexer",
            Self::Parser => "parser",
            Self::Sema => "sema",
            Self::Ir => "ir",
            Self::Obj => "obj",
            Self::Run => "run",
        }
    }
}

/// A stage capture request for one input source file.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub input: PathBuf,
    pub requested: BTreeSet<Stage>,
    pub opt_level: OptLevel,
}

impl CaptureRequest {
    pub fn new(input: impl Into<PathBuf>) -> Self {
        Self {
            input: input.into(),
            requested: BTreeSet::new(),
            opt_level: OptLevel::O0,
        }
    }

    pub fn with_stage(mut self, stage: Stage) -> Self {
        self.requested.insert(stage);
        self
    }

    pub fn with_all_stages(mut self) -> Self {
        self.requested.extend(Stage::ALL);
        self
    }

    pub fn with_opt_level(mut self, opt_level: OptLevel) -> Self {
        self.opt_level = opt_level;
        self
    }
}

/// Captured data for a single run.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub input: PathBuf,
    pub opt_level: OptLevel,
    pub stages: BTreeMap<Stage, CapturedStage>,
}

impl CaptureResult {
    pub fn get(&self, stage: Stage) -> Option<&CapturedStage> {
        self.stages.get(&stage)
    }
}

/// Failed stage capture with partial artifacts preserved.
#[derive(Debug, Clone)]
pub struct CaptureFailure {
    pub input: PathBuf,
    pub opt_level: OptLevel,
    pub stage: FailureStage,
    pub detail: String,
    pub stages: BTreeMap<Stage, CapturedStage>,
}

impl CaptureFailure {
    pub fn partial_result(&self) -> CaptureResult {
        CaptureResult {
            input: self.input.clone(),
            opt_level: self.opt_level,
            stages: self.stages.clone(),
        }
    }
}

impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage.as_str(), self.detail)
    }
}

impl std::error::Error for CaptureFailure {}

/// Captured artifact content.
#[derive(Debug, Clone)]
pub enum CapturedStage {
    Text(String),
    Run(RunCapture),
}

impl CapturedStage {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Run(_) => None,
        }
    }

    pub fn as_run(&self) -> Option<&RunCapture> {
        match self {
            Self::Text(_) => None,
            Self::Run(run) => Some(run),
        }
    }
}

/// Final executable output.
#[derive(Debug, Clone)]
pub struct RunCapture {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

/// Capture the requested stages for one source file.
pub fn capture_from_path(request: &CaptureRequest) -> Result<CaptureResult, CaptureFailure> {
    let mut stages = BTreeMap::new();
    let wants = |stage| request.requested.contains(&stage);
    let needs_backend = request.requested.iter().any(|stage| {
        matches!(stage, Stage::Mir | Stage::Regalloc | Stage::Asm | Stage::Obj | Stage::Run)
    });
    let input = request.input.clone();
    let source_form = detect_source_form(&input.to_string_lossy());

    let source = fs::read_to_string(&input).map_err(|e| CaptureFailure {
        input: input.clone(),
        opt_level: request.opt_level,
        stage: FailureStage::Preprocess,
        detail: format!("cannot read '{}': {}", input.display(), e),
        stages: stages.clone(),
    })?;

    let pp_config = crate::preprocess::PreprocConfig {
        filename: input.to_string_lossy().into_owned(),
        fixed_form: matches!(source_form, SourceForm::FixedForm),
        ..crate::preprocess::PreprocConfig::default()
    };
    let pp_result =
        crate::preprocess::preprocess(&source, &pp_config).map_err(|e| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Preprocess,
            detail: e.to_string(),
            stages: stages.clone(),
        })?;
    let preprocessed = pp_result.text;
    if wants(Stage::Preprocess) {
        stages.insert(Stage::Preprocess, CapturedStage::Text(preprocessed.clone()));
    }

    let tokens = tokenize(&preprocessed, 0, source_form).map_err(|e| CaptureFailure {
        input: input.clone(),
        opt_level: request.opt_level,
        stage: FailureStage::Lexer,
        detail: format!(
            "{}:{}: lexer error: {}",
            input.display(),
            e.span.start.line,
            e.msg
        ),
        stages: stages.clone(),
    })?;
    if wants(Stage::Tokens) {
        stages.insert(Stage::Tokens, CapturedStage::Text(format_tokens(&tokens)));
    }

    let mut parser = Parser::new(&tokens);
    let units = parser.parse_file().map_err(|e| CaptureFailure {
        input: input.clone(),
        opt_level: request.opt_level,
        stage: FailureStage::Parser,
        detail: format!(
            "{}:{}:{}: parse error: {}",
            input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        ),
        stages: stages.clone(),
    })?;
    if wants(Stage::Ast) {
        stages.insert(Stage::Ast, CapturedStage::Text(format!("{:#?}", units)));
    }

    let (st, type_layouts) = resolve::resolve_file(&units).map_err(|e| CaptureFailure {
        input: input.clone(),
        opt_level: request.opt_level,
        stage: FailureStage::Sema,
        detail: format!("{}:{}: {}", input.display(), e.span.start.line, e.msg),
        stages: stages.clone(),
    })?;
    let diags = validate::validate_file(&units, &st);
    if wants(Stage::Sema) {
        stages.insert(
            Stage::Sema,
            CapturedStage::Text(format_sema_snapshot(&st, &type_layouts, &diags)),
        );
    }
    let sema_errors: Vec<_> = diags
        .iter()
        .filter(|d| d.kind == validate::DiagKind::Error)
        .collect();
    if !sema_errors.is_empty() {
        return Err(CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Sema,
            detail: format_diagnostics(&input, &sema_errors),
            stages,
        });
    }

    let ir_module = lower::lower_file(&units, &st, &type_layouts);
    let ir_errors = verify::verify_module(&ir_module);
    if !ir_errors.is_empty() {
        let msg = ir_errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Ir,
            detail: format!("internal error: IR verification failed:\n{}", msg),
            stages,
        });
    }

    let ir_text = ir_printer::print_module(&ir_module);
    if wants(Stage::Ir) {
        stages.insert(Stage::Ir, CapturedStage::Text(ir_text.clone()));
    }
    let module_has_i128 = ir_module.contains_i128();
    let needs_optimized_pipeline =
        request.requested.iter().any(|stage| {
            matches!(
                stage,
                Stage::OptIr | Stage::Mir | Stage::Regalloc | Stage::Asm | Stage::Obj | Stage::Run
            )
        })
        && request.opt_level != OptLevel::O0;

    if ir_module.contains_i128_outside_globals() && needs_optimized_pipeline {
        return Err(CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Ir,
            detail: "integer(16) / i128 optimization is not yet supported; capture raw IR at O0 for now".into(),
            stages,
        });
    }

    let optimized_module = if needs_optimized_pipeline {
        let mut optimized = ir_module.clone();
        let ir_opt = match request.opt_level {
            OptLevel::O0 => IrOptLevel::O0,
            OptLevel::O1 => IrOptLevel::O1,
            OptLevel::O2 => IrOptLevel::O2,
            OptLevel::O3 => IrOptLevel::O3,
            OptLevel::Os => IrOptLevel::Os,
            OptLevel::Ofast => IrOptLevel::Ofast,
        };
        let pm = build_pipeline(ir_opt);
        pm.run(&mut optimized);
        Some(optimized)
    } else {
        None
    };

    if wants(Stage::OptIr) {
        let opt_ir_text = if let Some(module) = optimized_module.as_ref() {
            ir_printer::print_module(module)
        } else {
            ir_text.clone()
        };
        stages.insert(Stage::OptIr, CapturedStage::Text(opt_ir_text));
    }

    if !needs_backend {
        return Ok(CaptureResult {
            input,
            opt_level: request.opt_level,
            stages,
        });
    }

    let backend_ir = optimized_module.as_ref().unwrap_or(&ir_module);
    if module_has_i128 && !backend_ir.i128_backend_o0_supported() {
        return Err(CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Ir,
            detail: "backend does not yet support integer(16) / i128 codegen; capture IR at O0 for now".into(),
            stages,
        });
    }
    let machine_funcs = isel::select_module(backend_ir);
    if wants(Stage::Mir) {
        stages.insert(
            Stage::Mir,
            CapturedStage::Text(format_machine_functions(&machine_funcs)),
        );
    }

    let mut allocated = machine_funcs.clone();
    for mf in &mut allocated {
        let liveness = crate::codegen::liveness::compute_liveness(mf);
        let result = linearscan::linear_scan(mf);
        linearscan::apply_allocation(mf, &result, &liveness);
        linearscan::insert_callee_saves(mf, &result.callee_saved_used);
        linearscan::coalesce_moves(mf);
    }
    if wants(Stage::Regalloc) {
        stages.insert(
            Stage::Regalloc,
            CapturedStage::Text(format_machine_functions(&allocated)),
        );
    }

    let asm_text = emit_module_asm(backend_ir, &allocated);
    if wants(Stage::Asm) {
        stages.insert(Stage::Asm, CapturedStage::Text(asm_text.clone()));
    }

    if wants(Stage::Obj) || wants(Stage::Run) {
        let temp_root = next_temp_root("afs_tests_capture");
        let asm_path = temp_root.join("input.s");
        let obj_path = temp_root.join("output.o");
        let bin_path = temp_root.join("output");

        fs::create_dir_all(&temp_root).map_err(|e| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Obj,
            detail: format!("cannot create temp dir '{}': {}", temp_root.display(), e),
            stages: stages.clone(),
        })?;
        fs::write(&asm_path, &asm_text).map_err(|e| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Obj,
            detail: format!("cannot write temp assembly '{}': {}", asm_path.display(), e),
            stages: stages.clone(),
        })?;

        assemble_with_system(&asm_path, &obj_path).map_err(|detail| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Obj,
            detail,
            stages: stages.clone(),
        })?;

        if wants(Stage::Obj) {
            let snapshot = object_snapshot(&obj_path).map_err(|detail| CaptureFailure {
                input: input.clone(),
                opt_level: request.opt_level,
                stage: FailureStage::Obj,
                detail,
                stages: stages.clone(),
            })?;
            stages.insert(Stage::Obj, CapturedStage::Text(snapshot));
        }

        if wants(Stage::Run) {
            link_with_runtime(&obj_path, &bin_path).map_err(|detail| CaptureFailure {
                input: input.clone(),
                opt_level: request.opt_level,
                stage: FailureStage::Run,
                detail,
                stages: stages.clone(),
            })?;
            let sandbox = temp_root.join("run_sandbox");
            fs::create_dir_all(&sandbox).map_err(|e| CaptureFailure {
                input: input.clone(),
                opt_level: request.opt_level,
                stage: FailureStage::Run,
                detail: format!("cannot create run sandbox '{}': {}", sandbox.display(), e),
                stages: stages.clone(),
            })?;
            let output = Command::new(&bin_path)
                .current_dir(&sandbox)
                .output()
                .map_err(|e| CaptureFailure {
                    input: input.clone(),
                    opt_level: request.opt_level,
                    stage: FailureStage::Run,
                    detail: format!("cannot run '{}': {}", bin_path.display(), e),
                    stages: stages.clone(),
                })?;
            let files = snapshot_sandbox_files(&sandbox).map_err(|detail| CaptureFailure {
                input: input.clone(),
                opt_level: request.opt_level,
                stage: FailureStage::Run,
                detail,
                stages: stages.clone(),
            })?;
            stages.insert(
                Stage::Run,
                CapturedStage::Run(RunCapture {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    files,
                }),
            );
        }

        let _ = fs::remove_dir_all(&temp_root);
    }

    Ok(CaptureResult {
        input,
        opt_level: request.opt_level,
        stages,
    })
}

fn collect_sandbox_files(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_sandbox_files(root, &path, out)?;
        } else {
            let rel = path.strip_prefix(root).unwrap();
            out.insert(
                rel.to_string_lossy().replace('\\', "/"),
                fs::read(&path)?,
            );
        }
    }
    Ok(())
}

fn snapshot_sandbox_files(sandbox: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    collect_sandbox_files(sandbox, sandbox, &mut files)
        .map_err(|e| format!("cannot snapshot sandbox '{}': {}", sandbox.display(), e))?;
    Ok(files)
}

fn format_tokens(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        let _ = writeln!(
            out,
            "{}:{}-{}:{}\t{}\t{:?}",
            token.span.start.line,
            token.span.start.col,
            token.span.end.line,
            token.span.end.col,
            token.kind,
            token.text
        );
    }
    out
}

fn format_sema_snapshot(
    st: &crate::sema::symtab::SymbolTable,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    diags: &[validate::Diagnostic],
) -> String {
    let mut out = String::new();
    if diags.is_empty() {
        let _ = writeln!(out, "diagnostics: none");
    } else {
        let _ = writeln!(out, "diagnostics:");
        for diag in diags {
            let _ = writeln!(out, "  {}", diag);
        }
    }
    let _ = writeln!(out, "\nsymbol_table:\n{:#?}", st);
    let _ = writeln!(out, "\ntype_layouts:\n{:#?}", type_layouts);
    out
}

fn format_diagnostics(path: &Path, diags: &[&validate::Diagnostic]) -> String {
    diags
        .iter()
        .map(|diag| {
            format!(
                "{}:{}:{}: {}",
                path.display(),
                diag.span.start.line,
                diag.span.start.col,
                diag.msg
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_machine_functions(machine_funcs: &[MachineFunction]) -> String {
    let mut out = String::new();
    for (index, mf) in machine_funcs.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }

        let _ = writeln!(out, "function {}:", mf.name);
        let _ = writeln!(out, "  frame_size: {}", mf.frame.size);

        if mf.frame.locals.is_empty() {
            let _ = writeln!(out, "  locals: none");
        } else {
            let locals = mf
                .frame
                .locals
                .iter()
                .map(|slot| format!("[fp{:+}]:{}", slot.offset, slot.size))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "  locals: {}", locals);
        }

        if mf.vregs.is_empty() {
            let _ = writeln!(out, "  vregs: none");
        } else {
            let vregs = mf
                .vregs
                .iter()
                .map(|vreg| format!("v{}:{}", vreg.id.0, format_reg_class(vreg.class)))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "  vregs: {}", vregs);
        }

        for block in &mf.blocks {
            let _ = writeln!(out, "  block {}:", block.label);
            if block.insts.is_empty() {
                let _ = writeln!(out, "    <empty>");
            } else {
                for inst in &block.insts {
                    let _ = writeln!(out, "    {}", format_machine_inst(mf, inst));
                }
            }
        }

        if mf.const_pool.is_empty() {
            let _ = writeln!(out, "  const_pool: none");
        } else {
            let _ = writeln!(out, "  const_pool:");
            for (pool_index, entry) in mf.const_pool.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "    [{}] {}",
                    pool_index,
                    format_const_pool_entry(entry)
                );
            }
        }
    }
    out
}

fn format_machine_inst(mf: &MachineFunction, inst: &MachineInst) -> String {
    let opcode = format!("{:?}", inst.opcode).to_ascii_lowercase();
    let operands = inst
        .operands
        .iter()
        .map(|operand| format_machine_operand(mf, operand))
        .collect::<Vec<_>>();
    if operands.is_empty() {
        opcode
    } else {
        format!("{} {}", opcode, operands.join(", "))
    }
}

fn format_machine_operand(mf: &MachineFunction, operand: &MachineOperand) -> String {
    match operand {
        MachineOperand::VReg(vreg) => format!("v{}", vreg.0),
        MachineOperand::PhysReg(reg) => format_phys_reg(*reg),
        MachineOperand::Imm(value) => {
            if *value == -1 {
                "#frame-16".to_string()
            } else {
                format!("#{}", value)
            }
        }
        MachineOperand::FrameSlot(offset) => format!("[fp{:+}]", offset),
        MachineOperand::Cond(cond) => format_cond(*cond).to_string(),
        MachineOperand::BlockRef(block_id) => block_label(mf, *block_id),
        MachineOperand::Extern(name) => format!("extern {}", name),
        MachineOperand::ConstPool(index) => format!("constpool[{}]", index),
        MachineOperand::Shift(bits) => format!("lsl #{}", bits),
        MachineOperand::GlobalLabel(name) => format!("global:{}", name),
    }
}

fn format_phys_reg(reg: PhysReg) -> String {
    match reg {
        PhysReg::Gp(num) => format!("x{}", num),
        PhysReg::Gp32(num) => format!("w{}", num),
        PhysReg::Fp(num) => format!("d{}", num),
        PhysReg::Fp32(num) => format!("s{}", num),
        PhysReg::Sp => "sp".to_string(),
        PhysReg::Xzr => "xzr".to_string(),
        PhysReg::Wzr => "wzr".to_string(),
    }
}

fn format_reg_class(class: RegClass) -> &'static str {
    match class {
        RegClass::Gp64 => "gp64",
        RegClass::Gp32 => "gp32",
        RegClass::Fp64 => "fp64",
        RegClass::Fp32 => "fp32",
    }
}

fn format_cond(cond: ArmCond) -> &'static str {
    match cond {
        ArmCond::Eq => "eq",
        ArmCond::Ne => "ne",
        ArmCond::Hs => "hs",
        ArmCond::Lo => "lo",
        ArmCond::Mi => "mi",
        ArmCond::Pl => "pl",
        ArmCond::Hi => "hi",
        ArmCond::Ls => "ls",
        ArmCond::Ge => "ge",
        ArmCond::Lt => "lt",
        ArmCond::Gt => "gt",
        ArmCond::Le => "le",
    }
}

fn block_label(mf: &MachineFunction, block_id: MBlockId) -> String {
    mf.blocks
        .iter()
        .find(|block| block.id == block_id)
        .map(|block| block.label.clone())
        .unwrap_or_else(|| format!("block#{}", block_id.0))
}

fn format_const_pool_entry(entry: &ConstPoolEntry) -> String {
    match entry {
        ConstPoolEntry::F32(value) => format!("f32 {}", value),
        ConstPoolEntry::F64(value) => format!("f64 {}", value),
        ConstPoolEntry::I64(value) => format!("i64 {}", value),
        ConstPoolEntry::Bytes(bytes) => format!("bytes {:?}", escape_const_bytes(bytes)),
    }
}

fn escape_const_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b if b.is_ascii_graphic() || b == b' ' => out.push(b as char),
            other => {
                let _ = write!(out, "\\x{:02x}", other);
            }
        }
    }
    out
}

fn emit_module_asm(module: &crate::ir::inst::Module, allocated: &[MachineFunction]) -> String {
    let mut asm_text = String::new();
    asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
    for mf in allocated {
        asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
        asm_text.push_str(&emit::emit_function(mf));
        asm_text.push('\n');
    }

    if !module.globals.is_empty() {
        asm_text.push_str(&emit::emit_globals(&module.globals));
        asm_text.push('\n');
    }

    if let Some(user_func) = allocated.first() {
        if user_func.name != "main" {
            let _ = write!(
                asm_text,
                "\n.section __TEXT,__text,regular,pure_instructions\n\
.globl _main\n\
.p2align 2\n\
_main:\n\
    stp x29, x30, [sp, #-16]!\n\
    mov x29, sp\n\
    bl _afs_program_init\n\
    bl _{}\n\
    bl _afs_program_finalize\n\
    mov x0, #0\n\
    ldp x29, x30, [sp], #16\n\
    ret\n",
                user_func.name
            );
        }
    }

    asm_text
}

fn next_temp_root(prefix: &str) -> PathBuf {
    let id = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{}_{}_{}", prefix, std::process::id(), id))
}

fn assemble_with_system(asm_path: &Path, obj_path: &Path) -> Result<(), String> {
    let output = Command::new("as")
        .args([
            "-o",
            obj_path.to_str().unwrap_or("output.o"),
            asm_path.to_str().unwrap_or("input.s"),
        ])
        .output()
        .map_err(|e| format!("cannot run assembler: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "assembler failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn link_with_runtime(obj: &Path, output: &Path) -> Result<(), String> {
    let rt_path = find_runtime_lib()?;
    let sdk = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .map_err(|e| format!("cannot run xcrun: {}", e))?;
    if !sdk.status.success() {
        return Err(format!(
            "xcrun failed:\n{}",
            String::from_utf8_lossy(&sdk.stderr)
        ));
    }
    let sysroot = String::from_utf8_lossy(&sdk.stdout).trim().to_string();

    let ld = Command::new("ld")
        .args([
            obj.to_str().unwrap(),
            &rt_path,
            "-lSystem",
            "-no_uuid",
            "-syslibroot",
            &sysroot,
            "-e",
            "_main",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("cannot run linker: {}", e))?;
    if ld.status.success() {
        Ok(())
    } else {
        Err(format!(
            "linker failed:\n{}",
            String::from_utf8_lossy(&ld.stderr)
        ))
    }
}

fn find_runtime_lib() -> Result<String, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("libarmfortas_rt.a");
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }

    let candidates = [
        "target/debug/libarmfortas_rt.a",
        "target/release/libarmfortas_rt.a",
        "../target/debug/libarmfortas_rt.a",
        "../../target/debug/libarmfortas_rt.a",
    ];
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }

    Err("cannot find libarmfortas_rt.a — build with 'cargo build -p armfortas-rt'".into())
}

fn object_snapshot(path: &Path) -> Result<String, String> {
    let text = normalize_tool_output(&tool_output("otool", &["-t", path.to_str().unwrap()])?);
    let load = normalize_tool_output(&tool_output("otool", &["-l", path.to_str().unwrap()])?);
    let relocs = normalize_tool_output(&tool_output("otool", &["-rv", path.to_str().unwrap()])?);
    let symbols = normalize_tool_output(&tool_output("nm", &["-m", path.to_str().unwrap()])?);

    Ok(format!(
        "== text ==\n{}\n\n== load_commands ==\n{}\n\n== relocations ==\n{}\n\n== symbols ==\n{}",
        text, load, relocs, symbols
    ))
}

fn tool_output(tool: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {}", tool, e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "{} failed:\n{}",
            tool,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn normalize_tool_output(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_end().ends_with(".o:"))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}
