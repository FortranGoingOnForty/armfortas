//! Compilation driver.
//!
//! CLI argument parsing, phase orchestration, multi-file compilation,
//! dependency resolution, and linker invocation.

use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command;

use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::sema::{resolve, validate};
use crate::ir::{lower, verify, printer as ir_printer};
use crate::codegen::{isel, linearscan, emit};

/// Compilation options.
pub struct Options {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub emit_asm: bool,        // -S
    pub emit_obj: bool,        // -c
    pub emit_ir: bool,         // --emit-ir
    pub preprocess_only: bool, // -E
}

impl Options {
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut input = None;
        let mut output = None;
        let mut emit_asm = false;
        let mut emit_obj = false;
        let mut emit_ir = false;
        let mut preprocess_only = false;

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
                arg if !arg.starts_with('-') => {
                    input = Some(PathBuf::from(arg));
                }
                other => return Err(format!("unknown option: {}", other)),
            }
            i += 1;
        }

        let input = input.ok_or("no input file")?;
        Ok(Self { input, output, emit_asm, emit_obj, emit_ir, preprocess_only })
    }

    /// Determine the output path based on input and flags.
    pub fn output_path(&self) -> PathBuf {
        if let Some(ref o) = self.output {
            return o.clone();
        }
        let stem = self.input.file_stem().unwrap_or_default().to_str().unwrap_or("a");
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
    let pp_config = crate::preprocess::PreprocConfig {
        filename: opts.input.to_str().unwrap_or("<input>").to_string(),
        ..crate::preprocess::PreprocConfig::default()
    };
    let pp_result = crate::preprocess::preprocess(&source, &pp_config)
        .map_err(|e| format!("{}", e))?;
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
    let tokens = Lexer::tokenize(&preprocessed, 0)
        .map_err(|e| format!("{}:{}: lexer error: {}", opts.input.display(), e.span.start.line, e.msg))?;

    // 4. Parse.
    let mut parser = Parser::new(&tokens);
    let units = parser.parse_file()
        .map_err(|e| format!("{}:{}:{}: parse error: {}", opts.input.display(), e.span.start.line, e.span.start.col, e.msg))?;

    // 5. Semantic analysis.
    let st = resolve::resolve_file(&units)
        .map_err(|e| format!("{}:{}: {}", opts.input.display(), e.span.start.line, e.msg))?;
    let diags = validate::validate_file(&units, &st);
    for d in &diags {
        if d.kind == validate::DiagKind::Error {
            return Err(format!("{}:{}: error: {}", opts.input.display(), d.span.start.line, d.msg));
        }
    }

    // 6. Lower to IR.
    let ir_module = lower::lower_file(&units, &st);
    let ir_errors = verify::verify_module(&ir_module);
    if !ir_errors.is_empty() {
        let msg = ir_errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        return Err(format!("internal error: IR verification failed:\n{}", msg));
    }

    if opts.emit_ir {
        let ir_text = ir_printer::print_module(&ir_module);
        let out = opts.output_path();
        fs::write(&out, &ir_text)
            .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        return Ok(());
    }

    // 7. Instruction selection.
    let machine_funcs = isel::select_module(&ir_module);

    // 8. Register allocation (linear scan).
    let mut allocated: Vec<_> = machine_funcs;
    for mf in &mut allocated {
        let liveness = crate::codegen::liveness::compute_liveness(mf);
        let result = linearscan::linear_scan(mf);
        linearscan::apply_allocation(mf, &result, &liveness);
        linearscan::insert_callee_saves(mf, &result.callee_saved_used);
        linearscan::coalesce_moves(mf);
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

    // Emit _main entry point (must be in __TEXT section).
    if let Some(user_func) = allocated.first() {
        asm_text.push_str("\n.section __TEXT,__text,regular,pure_instructions\n");
        asm_text.push_str(&format!("\
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
", user_func.name));
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

    fs::write(&asm_path, &asm_text)
        .map_err(|e| format!("cannot write temp assembly: {}", e))?;

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
            "-syslibroot",
            &sysroot,
            "-e", "_main",
            "-o", output.to_str().unwrap(),
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
