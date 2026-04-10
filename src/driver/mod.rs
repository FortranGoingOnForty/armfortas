//! Compilation driver.
//!
//! CLI argument parsing, phase orchestration, multi-file compilation,
//! dependency resolution, and linker invocation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::codegen::{emit, isel, linearscan, peephole};
use crate::ir::{lower, printer as ir_printer, verify};
use crate::lexer::{detect_source_form, tokenize, SourceForm};
use crate::parser::Parser;
use crate::sema::{resolve, validate};

/// Optimization level requested at the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    Os,
    Ofast,
}

impl OptLevel {
    pub fn parse_flag(flag: &str) -> Option<Self> {
        match flag.to_ascii_lowercase().as_str() {
            "o0" => Some(Self::O0),
            "o1" => Some(Self::O1),
            "o2" => Some(Self::O2),
            "o3" => Some(Self::O3),
            "os" => Some(Self::Os),
            "ofast" => Some(Self::Ofast),
            _ => None,
        }
    }

    pub fn as_flag(&self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::O3 => "-O3",
            Self::Os => "-Os",
            Self::Ofast => "-Ofast",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::O0 => "O0",
            Self::O1 => "O1",
            Self::O2 => "O2",
            Self::O3 => "O3",
            Self::Os => "Os",
            Self::Ofast => "Ofast",
        }
    }
}

/// Compilation options.
pub struct Options {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub emit_asm: bool,        // -S
    pub emit_obj: bool,        // -c
    pub emit_ir: bool,         // --emit-ir
    pub preprocess_only: bool, // -E
    pub opt_level: OptLevel,   // -O0 .. -Ofast
}

impl Options {
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut input = None;
        let mut output = None;
        let mut emit_asm = false;
        let mut emit_obj = false;
        let mut emit_ir = false;
        let mut preprocess_only = false;
        let mut opt_level = OptLevel::O0;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-o" => {
                    i += 1;
                    if i < args.len() {
                        output = Some(PathBuf::from(&args[i]));
                    } else {
                        return Err("-o requires an argument".into());
                    }
                }
                "-S" => emit_asm = true,
                "-c" => emit_obj = true,
                "-E" => preprocess_only = true,
                "--emit-ir" => emit_ir = true,
                arg if arg.starts_with("-O") => {
                    let tail = &arg[1..];
                    opt_level = OptLevel::parse_flag(tail)
                        .ok_or_else(|| format!("unknown optimization level: {}", arg))?;
                }
                arg if !arg.starts_with('-') => {
                    input = Some(PathBuf::from(arg));
                }
                other => return Err(format!("unknown option: {}", other)),
            }
            i += 1;
        }

        let input = input.ok_or("no input file")?;
        Ok(Self {
            input,
            output,
            emit_asm,
            emit_obj,
            emit_ir,
            preprocess_only,
            opt_level,
        })
    }

    /// Determine the output path based on input and flags.
    pub fn output_path(&self) -> PathBuf {
        if let Some(ref o) = self.output {
            return o.clone();
        }
        let stem = self
            .input
            .file_stem()
            .unwrap_or_default()
            .to_str()
            .unwrap_or("a");
        if self.emit_asm {
            PathBuf::from(format!("{}.s", stem))
        } else if self.emit_obj {
            PathBuf::from(format!("{}.o", stem))
        } else if self.emit_ir {
            PathBuf::from(format!("{}.ir", stem))
        } else {
            PathBuf::from(stem)
        }
    }
}

/// Compile a Fortran source file through the full pipeline.
pub fn compile(opts: &Options) -> Result<(), String> {
    // 1. Read source.
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("cannot read '{}': {}", opts.input.display(), e))?;

    // 2. Preprocess.
    let source_form = detect_source_form(&opts.input.to_string_lossy());
    let pp_config = crate::preprocess::PreprocConfig {
        filename: opts.input.to_str().unwrap_or("<input>").to_string(),
        fixed_form: matches!(source_form, SourceForm::FixedForm),
        ..crate::preprocess::PreprocConfig::default()
    };
    let pp_result =
        crate::preprocess::preprocess(&source, &pp_config).map_err(|e| format!("{}", e))?;
    let preprocessed = pp_result.text;

    if opts.preprocess_only {
        let out = opts.output_path();
        if out.as_os_str() == "-" {
            print!("{}", preprocessed);
        } else {
            fs::write(&out, &preprocessed)
                .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        }
        return Ok(());
    }

    // 3. Lex.
    let tokens = tokenize(&preprocessed, 0, source_form).map_err(|e| {
        format!(
            "{}:{}:{}: lexer error: {}",
            opts.input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        )
    })?;

    // 4. Parse.
    let mut parser = Parser::new(&tokens);
    let units = parser.parse_file().map_err(|e| {
        format!(
            "{}:{}:{}: parse error: {}",
            opts.input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        )
    })?;

    // 5. Semantic analysis.
    let (st, type_layouts) = resolve::resolve_file(&units).map_err(|e| {
        format!(
            "{}:{}:{}: {}",
            opts.input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        )
    })?;
    let diags = validate::validate_file(&units, &st);
    for d in &diags {
        if d.kind == validate::DiagKind::Error {
            return Err(format!(
                "{}:{}:{}: error: {}",
                opts.input.display(),
                d.span.start.line,
                d.span.start.col,
                d.msg
            ));
        }
    }

    // 6. Lower to IR.
    let mut ir_module = lower::lower_file(&units, &st, &type_layouts);
    let ir_errors = verify::verify_module(&ir_module);
    if !ir_errors.is_empty() {
        let msg = ir_errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("internal error: IR verification failed:\n{}", msg));
    }
    let module_has_i128 = ir_module.contains_i128();
    // 6.5. Run IR optimization pipeline.
    //
    // This is where const_fold, mem2reg, LICM, DSE, loop unrolling, and
    // every other IR-level pass actually fire. At O0 the pipeline is empty
    // so nothing changes. The pipeline runs to fixpoint; the pass manager
    // verifies the IR after every pass.
    {
        use crate::opt::pipeline::OptLevel as IrOpt;
        let ir_opt = match opts.opt_level {
            OptLevel::O0    => IrOpt::O0,
            OptLevel::O1    => IrOpt::O1,
            OptLevel::O2    => IrOpt::O2,
            OptLevel::O3    => IrOpt::O3,
            OptLevel::Os    => IrOpt::Os,
            OptLevel::Ofast => IrOpt::Ofast,
        };
        let pm = if ir_module.contains_i128_outside_globals() && opts.opt_level != OptLevel::O0 {
            crate::opt::build_i128_pipeline(ir_opt).ok_or_else(|| {
                "integer(16) / i128 optimization above -O1 is not yet supported; use -O0 or -O1 for now"
                    .to_string()
            })?
        } else {
            crate::opt::build_pipeline(ir_opt)
        };
        pm.run(&mut ir_module);
    }

    if opts.emit_ir {
        let ir_text = ir_printer::print_module(&ir_module);
        let out = opts.output_path();
        fs::write(&out, &ir_text)
            .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        return Ok(());
    }

    if module_has_i128 && !ir_module.i128_backend_o0_supported() {
        return Err(
            "backend does not yet support integer(16) / i128 codegen; use --emit-ir for now"
                .into(),
        );
    }

    // 7. Instruction selection.
    let machine_funcs = isel::select_module(&ir_module);

    // 7.5. Backend peephole (O2+): FMA fusion, etc.
    let mut allocated: Vec<_> = machine_funcs;
    if opts.opt_level >= OptLevel::O2 {
        for mf in &mut allocated {
            peephole::run_peephole(mf);
        }
    }

    // 8. Register allocation (linear scan).
    for mf in &mut allocated {
        let liveness = crate::codegen::liveness::compute_liveness(mf);
        let result = linearscan::linear_scan(mf);
        linearscan::apply_allocation(mf, &result, &liveness);
        linearscan::insert_callee_saves(mf, &result.callee_saved_used);
        linearscan::coalesce_moves(mf);
        // 8.5. Tail call optimization (O1+): BL + epilogue → epilogue + B.
        // Runs after regalloc so we can inspect physical register assignments.
        if opts.opt_level >= OptLevel::O1 {
            crate::codegen::tailcall::tail_call_opt(mf);
        }
    }

    // 9. Emit assembly.
    let mut asm_text = String::new();
    asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
    for mf in &allocated {
        // Re-emit __TEXT section before each function in case the previous
        // function's constant pool switched to __DATA.
        asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
        asm_text.push_str(&emit::emit_function(mf));
        asm_text.push('\n');
    }

    // Emit module-level globals (SAVE'd locals + module variables)
    // into a __DATA,__data section. Must come before _main so the
    // labels are defined when functions reference them.
    if !ir_module.globals.is_empty() {
        asm_text.push_str(&emit::emit_globals(&ir_module.globals));
        asm_text.push('\n');
    }

    // Emit _main entry point (must be in __TEXT section).
    if let Some(user_func) = allocated.first() {
        if user_func.name != "main" {
            asm_text.push_str("\n.section __TEXT,__text,regular,pure_instructions\n");
            asm_text.push_str(&format!(
                "\
.globl _main
.p2align 2
_main:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl _afs_program_init
    bl _{0}
    bl _afs_program_finalize
    mov x0, #0
    ldp x29, x30, [sp], #16
    ret
",
                user_func.name
            ));
        }
    }

    if opts.emit_asm {
        let out = opts.output_path();
        fs::write(&out, &asm_text)
            .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        return Ok(());
    }

    // 10. Assemble (using system assembler for now).
    let pid = std::process::id();
    let asm_path = std::env::temp_dir().join(format!("armfortas_{}.s", pid));
    let obj_path = if opts.emit_obj {
        opts.output_path()
    } else {
        std::env::temp_dir().join(format!("armfortas_{}.o", pid))
    };

    fs::write(&asm_path, &asm_text).map_err(|e| format!("cannot write temp assembly: {}", e))?;

    let as_result = Command::new("as")
        .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("cannot run assembler: {}", e))?;

    if !as_result.status.success() {
        let stderr = String::from_utf8_lossy(&as_result.stderr);
        return Err(format!("assembler failed:\n{}", stderr));
    }

    if opts.emit_obj {
        return Ok(());
    }

    // 11. Link.
    let binary_path = opts.output_path();
    link(&obj_path, &binary_path)?;

    // Cleanup.
    let _ = fs::remove_file(&asm_path);
    let _ = fs::remove_file(&obj_path);

    Ok(())
}

/// Link an object file with the runtime library to produce a binary.
fn link(obj: &Path, output: &Path) -> Result<(), String> {
    // Find the runtime library.
    let rt_path = find_runtime_lib()?;

    // Find the SDK sysroot.
    let sdk = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .map_err(|e| format!("cannot run xcrun: {}", e))?;
    let sysroot = String::from_utf8_lossy(&sdk.stdout).trim().to_string();

    let ld_result = Command::new("ld")
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

    if !ld_result.status.success() {
        let stderr = String::from_utf8_lossy(&ld_result.stderr);
        return Err(format!("linker failed:\n{}", stderr));
    }

    Ok(())
}

/// Find libarmfortas_rt.a in common locations.
fn find_runtime_lib() -> Result<String, String> {
    // Check next to the compiler binary.
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let candidate = dir.join("libarmfortas_rt.a");
        if candidate.exists() {
            return Ok(candidate.to_str().unwrap().to_string());
        }
    }

    // Check cargo target directory (for development).
    let candidates = [
        "target/debug/libarmfortas_rt.a",
        "target/release/libarmfortas_rt.a",
        "../target/debug/libarmfortas_rt.a",
    ];
    for c in &candidates {
        if Path::new(c).exists() {
            return Ok(c.to_string());
        }
    }

    Err("cannot find libarmfortas_rt.a — build with 'cargo build -p armfortas-rt'".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_os_optimization_flag() {
        assert_eq!(OptLevel::parse_flag("Os"), Some(OptLevel::Os));
        assert_eq!(OptLevel::parse_flag("os"), Some(OptLevel::Os));
        assert_eq!(OptLevel::Os.as_flag(), "-Os");
        assert_eq!(OptLevel::Os.as_str(), "Os");
    }

    #[test]
    fn options_from_args_accepts_os() {
        let args = vec!["-Os".to_string(), "hello.f90".to_string()];
        let opts = Options::from_args(&args).expect("driver should accept -Os");
        assert_eq!(opts.opt_level, OptLevel::Os);
        assert_eq!(opts.input, PathBuf::from("hello.f90"));
    }

    fn i128_fixture() -> PathBuf {
        let path = PathBuf::from("tests/fixtures").join("integer16_ir.f90");
        assert!(path.exists(), "missing test fixture {}", path.display());
        path
    }

    fn i128_reject_fixture() -> PathBuf {
        let path = PathBuf::from("tests/fixtures").join("integer16_mul.f90");
        assert!(path.exists(), "missing test fixture {}", path.display());
        path
    }

    fn i128_internal_call_fixture() -> PathBuf {
        let path = PathBuf::from("tests/fixtures").join("integer16_internal_call.f90");
        assert!(path.exists(), "missing test fixture {}", path.display());
        path
    }

    fn i128_external_call_fixture() -> PathBuf {
        let path = PathBuf::from("tests/fixtures").join("integer16_external_call.f90");
        assert!(path.exists(), "missing test fixture {}", path.display());
        path
    }

    #[test]
    fn emit_ir_allows_integer16_staging_at_o0() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_ir_{}_{}.ir",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: i128_fixture(),
            output: Some(output.clone()),
            emit_asm: false,
            emit_obj: false,
            emit_ir: true,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        compile(&opts).expect("O0 --emit-ir should support integer(16) staging");
        let ir = fs::read_to_string(&output).expect("missing emitted IR");
        assert!(ir.contains("i128"), "emitted IR should expose integer(16) as i128:\n{}", ir);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn backend_rejects_integer16_arithmetic_for_now() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_bin_{}_{}",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: i128_reject_fixture(),
            output: Some(output),
            emit_asm: false,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        let err = compile(&opts).expect_err("backend should reject integer(16) until i128 codegen lands");
        assert!(
            err.contains("backend does not yet support integer(16) / i128 codegen"),
            "unexpected backend rejection:\n{}",
            err
        );
    }

    #[test]
    fn backend_allows_simple_integer16_memory_codegen_at_o0() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_mem_{}_{}.s",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: i128_fixture(),
            output: Some(output.clone()),
            emit_asm: true,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        compile(&opts).expect("simple integer(16) memory traffic should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("stp x16, x17"), "expected paired i128 store in asm:\n{}", asm);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn backend_allows_simple_integer16_add_codegen_at_o0() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_add_{}_{}.s",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: PathBuf::from("tests/fixtures").join("integer16_add.f90"),
            output: Some(output.clone()),
            emit_asm: true,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        compile(&opts).expect("simple integer(16) add should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("adds x16, x16, x8"), "expected i128 add carry chain in asm:\n{}", asm);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn backend_allows_internal_integer16_call_codegen_at_o0() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_call_{}_{}.s",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: i128_internal_call_fixture(),
            output: Some(output.clone()),
            emit_asm: true,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        compile(&opts).expect("internal integer(16) call should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("bl _add_one"), "expected internal helper call in asm:\n{}", asm);
        assert!(asm.contains("stp x0, x1"), "expected pair-register i128 ABI spill in asm:\n{}", asm);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn backend_allows_external_integer16_call_codegen_at_o0() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_external_call_{}_{}.s",
            std::process::id(),
            "o0"
        ));
        let opts = Options {
            input: i128_external_call_fixture(),
            output: Some(output.clone()),
            emit_asm: true,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O0,
        };

        compile(&opts).expect("external integer(16) call should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("bl _add_ext"), "expected external helper call in asm:\n{}", asm);
        assert!(asm.contains("stp x0, x1"), "expected pair-register i128 ABI spill in asm:\n{}", asm);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn backend_allows_integer16_mul_after_o1_const_fold() {
        let output = std::env::temp_dir().join(format!(
            "armfortas_i128_mul_{}_{}.s",
            std::process::id(),
            "o1"
        ));
        let opts = Options {
            input: i128_reject_fixture(),
            output: Some(output.clone()),
            emit_asm: true,
            emit_obj: false,
            emit_ir: false,
            preprocess_only: false,
            opt_level: OptLevel::O1,
        };

        compile(&opts).expect("integer(16) multiply should codegen at O1 after const fold");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("movz x16, #42"), "expected folded i128 constant in asm:\n{}", asm);
        assert!(!asm.contains("mul "), "expected O1 i128 multiply to fold away before backend:\n{}", asm);
        let _ = fs::remove_file(output);
    }
}
