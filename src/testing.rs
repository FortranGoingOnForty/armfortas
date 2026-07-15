//! Bench-facing capture helpers.
//!
//! This module exposes a stable-ish API for collecting intermediate artifacts
//! from the compiler pipeline so the external `afs-tests` runner can assert on
//! more than just final program output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::codegen::mir::{
    ArmCond, ConstPoolEntry, MBlockId, MachineFunction, MachineInst, MachineOperand, PhysReg,
    RegClass,
};
use crate::codegen::{emit, isel, linearscan};
use crate::driver::OptLevel;
use crate::ir::{lower, printer as ir_printer, verify};
use crate::lexer::{detect_source_form, tokenize_source_view, SourceForm, Token};
use crate::opt::pipeline::OptLevel as IrOptLevel;
use crate::opt::{build_i128_pipeline, build_pipeline};
use crate::parser::Parser;
use crate::runtime::artifact::{fresh_runtime_lib, runtime_lib_candidate, RuntimeProfile};
use crate::sema::{resolve, validate};

/// Return the profile directory that contains the running Cargo test.
///
/// Integration tests live in `<profile>/deps`; workspace binaries and static
/// libraries are siblings of that directory. Deriving this from the running
/// executable also handles custom target directories and Cargo configuration.
pub fn cargo_profile_dir() -> Option<PathBuf> {
    cargo_profile_dir_for_executable(&std::env::current_exe().ok()?)
}

fn cargo_profile_dir_for_executable(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    if parent.file_name() == Some(std::ffi::OsStr::new("deps")) {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

/// Find a workspace binary built in the same Cargo profile as this test.
pub fn built_binary(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(format!("CARGO_BIN_EXE_{name}")) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let file_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    built_artifact(&file_name)
}

/// Find a workspace artifact built in the same Cargo profile as this test.
pub fn built_artifact(file_name: &str) -> Option<PathBuf> {
    let candidate = cargo_profile_dir()?.join(file_name);
    candidate.is_file().then_some(candidate)
}

/// Find the runtime archive built in the same Cargo profile as this test.
pub fn built_runtime_archive() -> Option<PathBuf> {
    built_artifact("libarmfortas_rt.a")
}

fn harness_skip_line(suite: &str, test: &str, count: usize, reason: &str) -> String {
    format!("\nHARNESS_SKIP suite={suite} test={test} count={count} reason=\"{reason}\"\n")
}

/// Report a platform-gated test as one write so parallel libtest output cannot
/// split the machine-readable accounting record.
pub fn report_harness_skip(suite: &str, test: &str, count: usize, reason: &str) {
    assert!(count > 0, "HARNESS_SKIP counts must be positive");
    let line = harness_skip_line(suite, test, count, reason);
    std::io::stderr()
        .lock()
        .write_all(line.as_bytes())
        .expect("write HARNESS_SKIP record");
}

#[cfg(test)]
mod harness_skip_tests {
    use super::harness_skip_line;

    #[test]
    fn record_is_rendered_as_one_complete_line() {
        assert_eq!(
            harness_skip_line("suite", "case", 7, "platform gate"),
            "\nHARNESS_SKIP suite=suite test=case count=7 reason=\"platform gate\"\n"
        );
    }
}

/// Whether this host can assemble, link, and run armfortas-produced
/// binaries natively (sprint x01). arm64-macos always; x86_64 ELF
/// glibc/FreeBSD since x09 opened the optimizing levels (x06 built the
/// link path, x07/x08 proved -O0 and the ABI hard cases). E2e suites
/// call this before compiling anything; on `Err` they print one
/// `HARNESS_SKIP` line per `#[test]` and return. The error text is the
/// skip reason.
pub fn native_e2e_support() -> Result<(), String> {
    let host = crate::target::TargetSpec::host();
    if host == crate::target::TargetSpec::parse("arm64-macos").unwrap() {
        return Ok(());
    }
    if host.arch == crate::target::Arch::X86_64
        && host.object_format() == crate::target::ObjectFormat::Elf
        && host.libc != crate::target::Libc::Musl
    {
        return Ok(());
    }
    Err(format!(
        "host {} has no native run path (musl linking lands in x11)",
        host
    ))
}

/// Gate for suites that assert Mach-O artifact properties (otool/nm
/// snapshots, LC_UUID, load commands). These are format-specific
/// audits, not portability gaps — ELF twins would assert different
/// things (build-id, readelf sections) and belong to whichever sprint
/// needs them.
pub fn native_macho_toolchain_support() -> Result<(), String> {
    let host = crate::target::TargetSpec::host();
    if host == crate::target::TargetSpec::parse("arm64-macos").unwrap() {
        Ok(())
    } else {
        Err(format!(
            "host {}: Mach-O artifact audits need the arm64-macos toolchain",
            host
        ))
    }
}

/// Gate for suites that assert vectorized IR/asm shapes: NEON shapes
/// on arm64-macos, SSE2 shapes on x86_64 ELF glibc/FreeBSD hosts since
/// x10 registered SseVectorize at O3/Ofast. Shapes the SSE2 legality
/// table refuses (src/opt/vec_isa.rs) stay scalar on x86; suites
/// assert per-arch. musl waits on the x11 link story.
pub fn native_vectorizer_support() -> Result<(), String> {
    let host = crate::target::TargetSpec::host();
    if host == crate::target::TargetSpec::parse("arm64-macos").unwrap() {
        return Ok(());
    }
    if host.arch == crate::target::Arch::X86_64
        && host.object_format() == crate::target::ObjectFormat::Elf
        && host.libc != crate::target::Libc::Musl
    {
        return Ok(());
    }
    Err(format!(
        "host {} has no native run path (musl linking lands in x11)",
        host
    ))
}

/// Level-aware gate for the run_programs sweep: macOS and x86_64 ELF
/// glibc/FreeBSD hosts run every opt level (x09 opened -O1..-Ofast
/// after the vectorizer gating landed and the level sweeps came back
/// clean); musl waits for the x11 link story.
pub fn native_e2e_level_support(opt_flag: &str) -> Result<(), String> {
    let _ = opt_flag;
    let host = crate::target::TargetSpec::host();
    if host == crate::target::TargetSpec::parse("arm64-macos").unwrap() {
        return Ok(());
    }
    if host.arch == crate::target::Arch::X86_64
        && host.object_format() == crate::target::ObjectFormat::Elf
        && host.libc != crate::target::Libc::Musl
    {
        return Ok(());
    }
    Err(format!(
        "host {} has no native run path (musl linking lands in x11)",
        host
    ))
}

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
        matches!(
            stage,
            Stage::Mir | Stage::Regalloc | Stage::Asm | Stage::Obj | Stage::Run
        )
    });
    let input = request.input.clone();
    let source_form = detect_source_form(&input.to_string_lossy());

    let source = fs::read(&input).map_err(|e| CaptureFailure {
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
        crate::preprocess::preprocess_bytes(&source, &pp_config).map_err(|e| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Preprocess,
            detail: e.to_string(),
            stages: stages.clone(),
        })?;
    let preprocessed = pp_result.text.clone();
    if wants(Stage::Preprocess) {
        stages.insert(
            Stage::Preprocess,
            CapturedStage::Text(String::from_utf8_lossy(&pp_result.bytes()).into_owned()),
        );
    }

    let tokens = tokenize_source_view(&preprocessed, 0, source_form).map_err(|e| {
        let resolved = pp_result.resolve_span(e.span);
        CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Lexer,
            detail: format!(
                "{}:{}:{}: lexer error: {}",
                resolved.filename,
                resolved.display_span.start.line,
                resolved.display_span.start.col,
                e.msg
            ),
            stages: stages.clone(),
        }
    })?;
    if wants(Stage::Tokens) {
        stages.insert(Stage::Tokens, CapturedStage::Text(format_tokens(&tokens)));
    }

    let mut parser = Parser::new_source_view(&tokens);
    let units = parser.parse_file().map_err(|e| {
        let resolved = pp_result.resolve_span(e.span);
        CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Parser,
            detail: format!(
                "{}:{}:{}: parse error: {}",
                resolved.filename,
                resolved.display_span.start.line,
                resolved.display_span.start.col,
                e.msg
            ),
            stages: stages.clone(),
        }
    })?;
    if wants(Stage::Ast) {
        stages.insert(Stage::Ast, CapturedStage::Text(format!("{:#?}", units)));
    }

    let rr = resolve::resolve_file(
        &units,
        &[],
        crate::target::TargetLayout::of(&crate::target::TargetSpec::host()),
    )
    .map_err(|e| {
        let resolved = pp_result.resolve_span(e.span);
        CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Sema,
            detail: format!(
                "{}:{}:{}: {}",
                resolved.filename,
                resolved.display_span.start.line,
                resolved.display_span.start.col,
                e.msg
            ),
            stages: stages.clone(),
        }
    })?;
    let st = rr.st;
    let type_layouts = rr.type_layouts;
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
            detail: format_diagnostics(&pp_result, &sema_errors),
            stages,
        });
    }

    let (ir_module, _module_globals) = lower::lower_file(
        &units,
        &st,
        &type_layouts,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        crate::target::TargetLayout::of(&crate::target::TargetSpec::host()),
    );
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
    let needs_optimized_pipeline = request.requested.iter().any(|stage| {
        matches!(
            stage,
            Stage::OptIr | Stage::Mir | Stage::Regalloc | Stage::Asm | Stage::Obj | Stage::Run
        )
    }) && request.opt_level != OptLevel::O0;

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
        // Captures compile for the host target (the layout calls above
        // use TargetSpec::host() too), so the pipeline gates on the
        // host arch.
        let host_arch = crate::target::TargetSpec::host().arch;
        let pm = if ir_module.contains_i128_outside_globals() && request.opt_level != OptLevel::O0 {
            build_i128_pipeline(ir_opt, host_arch).ok_or_else(|| CaptureFailure {
                input: input.clone(),
                opt_level: request.opt_level,
                stage: FailureStage::Ir,
                detail: format!(
                    "integer(16) / i128 optimization at {} is not yet supported; capture raw IR or use a supported opt level",
                    request.opt_level.as_flag()
                ),
                stages: stages.clone(),
            })?
        } else {
            build_pipeline(ir_opt, host_arch)
        };
        pm.run(&mut optimized).map_err(|error| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Ir,
            detail: error.to_string(),
            stages: stages.clone(),
        })?;
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
            detail:
                "backend does not yet support integer(16) / i128 codegen; capture IR at O0 for now"
                    .into(),
            stages,
        });
    }
    // The backend stages follow the HOST target: on arm64-macos the
    // hand-rolled ARM sequence below mirrors the driver pipeline
    // stage by stage; on x86_64 ELF hosts (x09) the x86 backend runs
    // instead — captured Mir/Regalloc are x86 MIR dumps and the asm
    // comes from the same `codegen::emit_module` entry the driver
    // uses, so captured asm matches shipped asm on both targets.
    let host = crate::target::TargetSpec::host();
    let asm_text = if host.arch == crate::target::Arch::X86_64 {
        let mut funcs = crate::codegen::x86::isel::select_module(backend_ir);
        if wants(Stage::Mir) {
            stages.insert(
                Stage::Mir,
                CapturedStage::Text(format_x86_functions(&funcs)),
            );
        }
        for f in &mut funcs {
            if request.opt_level >= OptLevel::O2 {
                crate::codegen::x86::peephole::run_peephole(f);
            }
            crate::codegen::x86::twoaddr::convert_to_two_address(f);
            crate::codegen::x86::regalloc::regalloc_naive(f);
        }
        if wants(Stage::Regalloc) {
            stages.insert(
                Stage::Regalloc,
                CapturedStage::Text(format_x86_functions(&funcs)),
            );
        }
        let host_opts = crate::driver::Options {
            opt_level: request.opt_level,
            ..Default::default()
        };
        crate::codegen::emit_module(backend_ir, &host_opts).map_err(|detail| CaptureFailure {
            input: input.clone(),
            opt_level: request.opt_level,
            stage: FailureStage::Ir,
            detail,
            stages: stages.clone(),
        })?
    } else {
        let machine_funcs = isel::select_module(backend_ir);
        if wants(Stage::Mir) {
            stages.insert(
                Stage::Mir,
                CapturedStage::Text(format_machine_functions(&machine_funcs)),
            );
        }

        let mut allocated = machine_funcs.clone();
        // Backend peephole at O2+ (must run BEFORE regalloc because it
        // operates on vregs); floating-point contraction is Ofast-only.
        if request.opt_level >= OptLevel::O2 {
            for mf in &mut allocated {
                crate::codegen::peephole::run_peephole(mf, request.opt_level.fp_contract());
            }
        }
        for mf in &mut allocated {
            if request.opt_level == OptLevel::O0 {
                crate::codegen::regalloc::regalloc_naive(mf);
            } else {
                let liveness = crate::codegen::liveness::compute_liveness(mf);
                let result = linearscan::linear_scan(mf, &liveness);
                linearscan::apply_allocation(mf, &result, &liveness);
                // Post-allocation passes — must mirror the driver pipeline so
                // captured asm matches the binary the user actually ships.
                // parallelize_call_arg_moves in particular fixes a w0/w1
                // clobber pattern visible at high register pressure.
                linearscan::parallelize_entry_arg_moves(mf);
                linearscan::parallelize_call_arg_moves(mf);
                linearscan::insert_split_bridges(mf, &result.split_records);
                linearscan::insert_callee_saves(mf, &result.callee_saved_used);
                linearscan::coalesce_moves(mf);
                crate::codegen::tailcall::tail_call_opt(mf);
            }
            crate::codegen::relax_branches::relax_branches(mf);
        }
        if wants(Stage::Regalloc) {
            stages.insert(
                Stage::Regalloc,
                CapturedStage::Text(format_machine_functions(&allocated)),
            );
        }

        emit_module_asm(backend_ir, &allocated)
    };
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
            out.insert(rel.to_string_lossy().replace('\\', "/"), fs::read(&path)?);
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

fn format_diagnostics(
    preprocessed: &crate::preprocess::PreprocOutput,
    diags: &[&validate::Diagnostic],
) -> String {
    diags
        .iter()
        .map(|diag| {
            let resolved = preprocessed.resolve_span(diag.span);
            format!(
                "{}:{}:{}: {}",
                resolved.filename,
                resolved.display_span.start.line,
                resolved.display_span.start.col,
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
        RegClass::V128 => "v128",
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
        asm_text.push_str(&crate::codegen::shared::emit_globals(
            &module.globals,
            &module.layout,
            crate::codegen::shared::GlobalsDialect::MachO,
        ));
        asm_text.push('\n');
    }

    if let Some(user_func) = main_wrapper_target(allocated) {
        if user_func != "main" {
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
                user_func
            );
        }
    }

    asm_text
}

fn main_wrapper_target(allocated: &[MachineFunction]) -> Option<&str> {
    allocated
        .iter()
        .find(|func| func.name.starts_with("__prog_"))
        .or_else(|| allocated.iter().find(|func| func.name != "main"))
        .map(|func| func.name.as_str())
}

fn next_temp_root(prefix: &str) -> PathBuf {
    let id = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{}_{}_{}", prefix, std::process::id(), id))
}

/// Debug-format x86 machine functions for the Mir/Regalloc capture
/// stages. A structured printer (MirView) is x10's call; until then a
/// derive(Debug) dump is honest and greppable.
fn format_x86_functions(funcs: &[crate::codegen::x86::mir::X86Function]) -> String {
    let mut out = String::new();
    for f in funcs {
        out.push_str(&format!("{:#?}\n", f));
    }
    out
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

/// Link a captured object into a runnable binary for the HOST: the
/// driver's ELF link line on x86_64 ELF hosts, the Mach-O xcrun/ld
/// path on arm64-macos.
fn link_with_runtime(obj: &Path, output: &Path) -> Result<(), String> {
    let host = crate::target::TargetSpec::host();
    if host.object_format() == crate::target::ObjectFormat::Elf {
        // Refresh the runtime staticlib the same way the Mach-O path
        // does before delegating to the driver's link line.
        if let Some(workspace_root) = find_workspace_root() {
            maybe_refresh_runtime_lib(&workspace_root, RuntimeProfile::current())?;
        }
        let opts = crate::driver::Options::default();
        return crate::driver::link_inputs_elf(&[obj.to_path_buf()], output, &opts);
    }
    link_with_runtime_macho(obj, output)
}

fn link_with_runtime_macho(obj: &Path, output: &Path) -> Result<(), String> {
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
    if let Some(workspace_root) = find_workspace_root() {
        let profile = RuntimeProfile::current();
        if let Some(candidate) = fresh_runtime_lib(&workspace_root, profile) {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        maybe_refresh_runtime_lib(&workspace_root, profile)?;
        let candidate = runtime_lib_candidate(&workspace_root, profile);
        if candidate.exists() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("libarmfortas_rt.a");
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }

    Err("cannot find libarmfortas_rt.a — build with 'cargo build -p armfortas-rt'".into())
}

fn maybe_refresh_runtime_lib(workspace_root: &Path, profile: RuntimeProfile) -> Result<(), String> {
    let runtime_dir = workspace_root.join("runtime");
    if !runtime_dir.join("Cargo.toml").exists() {
        return Ok(());
    }

    if fresh_runtime_lib(workspace_root, profile).is_some() {
        return Ok(());
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root)
        .args(profile.cargo_build_args())
        .output()
        .map_err(|e| format!("cannot rebuild libarmfortas_rt.a: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cannot rebuild libarmfortas_rt.a:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn find_workspace_root() -> Option<PathBuf> {
    let mut bases = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            bases.push(dir.to_path_buf());
        }
    }

    for base in bases {
        for ancestor in base.ancestors() {
            if ancestor.join("Cargo.toml").exists() && ancestor.join("runtime/Cargo.toml").exists()
            {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    None
}

/// Object-file inspection dispatched on object format (sprint x01).
/// Mach-O goes through Apple's otool/nm; ELF through objdump/readelf/nm.
/// llvm-prefixed tool names are preferred on ELF so FreeBSD base and
/// Alpine resolve to the same implementation. Snapshots are compared
/// within a single host run only — never across tools or platforms — so
/// cross-tool formatting differences are harmless and no cross-tool
/// golden files may be introduced on top of this.
pub struct ObjectInspector {
    format: crate::target::ObjectFormat,
    objdump: String,
    readelf: String,
}

impl ObjectInspector {
    pub fn for_format(format: crate::target::ObjectFormat) -> Self {
        ObjectInspector {
            format,
            objdump: find_inspection_tool("AFS_OBJDUMP_BIN", &["llvm-objdump", "objdump"]),
            readelf: find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]),
        }
    }

    /// Four named captures: text, load commands/headers, relocations,
    /// symbols. The Mach-O argv is byte-compatible with the pre-x01
    /// `object_snapshot`. `nm -m` is Apple-nm-only; the ELF side uses
    /// plain `nm`.
    pub fn snapshot(&self, path: &Path) -> Result<String, String> {
        let p = path.to_str().unwrap();
        let (text, load, relocs, symbols) = match self.format {
            crate::target::ObjectFormat::MachO => (
                tool_output("otool", &["-t", p])?,
                tool_output("otool", &["-l", p])?,
                tool_output("otool", &["-rv", p])?,
                tool_output("nm", &["-m", p])?,
            ),
            crate::target::ObjectFormat::Elf => (
                tool_output(&self.objdump, &["-d", p])?,
                tool_output(&self.readelf, &["-lSW", p])?,
                tool_output(&self.objdump, &["-r", p])?,
                tool_output("nm", &[p])?,
            ),
        };
        Ok(format!(
            "== text ==\n{}\n\n== load_commands ==\n{}\n\n== relocations ==\n{}\n\n== symbols ==\n{}",
            normalize_tool_output(&text),
            normalize_tool_output(&load),
            normalize_tool_output(&relocs),
            normalize_tool_output(&symbols),
        ))
    }
}

/// Resolve an ELF inspection tool: env override first, then the first
/// candidate on PATH that answers `--version`.
pub fn find_inspection_tool(env_key: &str, candidates: &[&str]) -> String {
    if let Some(over) = std::env::var_os(env_key) {
        return over.to_string_lossy().into_owned();
    }
    for candidate in candidates {
        let probe = Command::new(candidate)
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if probe {
            return candidate.to_string();
        }
    }
    // Fall through to the last candidate; the eventual spawn error
    // names the missing tool, which beats failing here without context.
    candidates.last().unwrap().to_string()
}

fn object_snapshot(path: &Path) -> Result<String, String> {
    // Captures compile for the host (x09), so inspect with the host's
    // object format.
    let format = crate::target::TargetSpec::host().object_format();
    ObjectInspector::for_format(format).snapshot(path)
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
        // objdump's ELF header line carries the temp path
        // ("/tmp/.../output.o:\tfile format elf64-x86-64"); otool's
        // path line is bare and caught by the ".o:" filter above.
        .filter(|line| !(line.contains(".o:") && line.contains("file format")))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod inspector_tests {
    use super::*;

    #[test]
    fn tool_resolution_env_override_wins() {
        // Env overrides are read at construction; use a process-unique
        // key value and restore afterwards (tests share the process).
        std::env::set_var("AFS_OBJDUMP_BIN", "/nonexistent/custom-objdump");
        let tool = find_inspection_tool("AFS_OBJDUMP_BIN", &["llvm-objdump", "objdump"]);
        std::env::remove_var("AFS_OBJDUMP_BIN");
        assert_eq!(tool, "/nonexistent/custom-objdump");
    }

    #[test]
    fn tool_resolution_falls_back_to_last_candidate() {
        let tool = find_inspection_tool(
            "AFS_TEST_NO_SUCH_ENV_KEY",
            &["definitely-not-a-real-tool-xyz", "also-not-real-abc"],
        );
        assert_eq!(tool, "also-not-real-abc");
    }

    /// DoD golden (x01): on a host with the native toolchain, a fixed
    /// .o produces byte-identical snapshots from the new inspector and
    /// the pre-sprint otool/nm pipeline spelled out literally.
    #[test]
    fn macho_snapshot_matches_pre_sprint_pipeline() {
        // Mach-O-specific golden: requires the arm64-macos toolchain,
        // not just any native run path (x09 opened native_e2e_support
        // to ELF hosts, where otool/Mach-O semantics don't exist).
        if native_macho_toolchain_support().is_err() {
            eprintln!(
                "\nHARNESS_SKIP suite=testing test=macho_snapshot_matches_pre_sprint_pipeline count=1 reason=\"Mach-O tools need the arm64-macos toolchain\""
            );
            return;
        }
        let req = CaptureRequest::new("tests/fixtures/integer16_add.f90")
            .with_stage(Stage::Obj)
            .with_opt_level(OptLevel::O0);
        // Stage::Obj capture itself calls the new path; what we need is
        // the .o on disk, so assemble via the capture pipeline's
        // internals: compile to asm, then system as.
        let result = capture_from_path(&req).expect("Obj capture should succeed on macOS");
        let _ = result; // capture proves the new path runs; now compare on a fresh object
        let dir = std::env::temp_dir().join(format!("afs_inspector_golden_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let asm = dir.join("g.s");
        let obj = dir.join("g.o");
        let cap = CaptureRequest::new("tests/fixtures/integer16_add.f90")
            .with_stage(Stage::Asm)
            .with_opt_level(OptLevel::O0);
        let asm_text = capture_from_path(&cap)
            .expect("asm capture")
            .get(Stage::Asm)
            .and_then(|s| s.as_text().map(str::to_string))
            .expect("asm text");
        fs::write(&asm, asm_text).unwrap();
        let status = Command::new("as")
            .args(["-o", obj.to_str().unwrap(), asm.to_str().unwrap()])
            .status()
            .expect("system as");
        assert!(status.success());

        let new = ObjectInspector::for_format(crate::target::ObjectFormat::MachO)
            .snapshot(&obj)
            .expect("inspector snapshot");
        // Pre-sprint pipeline, verbatim.
        let p = obj.to_str().unwrap();
        let old = format!(
            "== text ==\n{}\n\n== load_commands ==\n{}\n\n== relocations ==\n{}\n\n== symbols ==\n{}",
            normalize_tool_output(&tool_output("otool", &["-t", p]).unwrap()),
            normalize_tool_output(&tool_output("otool", &["-l", p]).unwrap()),
            normalize_tool_output(&tool_output("otool", &["-rv", p]).unwrap()),
            normalize_tool_output(&tool_output("nm", &["-m", p]).unwrap()),
        );
        assert_eq!(
            new, old,
            "inspector must be byte-compatible with the pre-x01 snapshot"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
