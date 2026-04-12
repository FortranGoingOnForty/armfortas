//! Compilation driver.
//!
//! CLI argument parsing, phase orchestration, multi-file compilation,
//! dependency resolution, and linker invocation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::codegen::{emit, isel, linearscan, peephole};
use crate::codegen::mir::MachineFunction;
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
    /// Directories to search for `.amod` module files (`-I <dir>`).
    pub module_search_paths: Vec<PathBuf>,
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
        let mut module_search_paths = Vec::new();

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
                "-I" => {
                    i += 1;
                    if i < args.len() {
                        module_search_paths.push(PathBuf::from(&args[i]));
                    } else {
                        return Err("-I requires a directory argument".into());
                    }
                }
                arg if arg.starts_with("-I") => {
                    // -Idir (no space)
                    module_search_paths.push(PathBuf::from(&arg[2..]));
                }
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
            module_search_paths,
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

fn main_wrapper_target(allocated: &[MachineFunction]) -> Option<&str> {
    // Only emit _main if there's a __prog_* function (a Fortran PROGRAM
    // body).  The previous .or_else fallback picked any non-"main"
    // function, which incorrectly wrapped module procedures.
    allocated
        .iter()
        .find(|func| func.name.starts_with("__prog_"))
        .map(|func| func.name.as_str())
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
    let resolve_result = resolve::resolve_file(&units, &opts.module_search_paths).map_err(|e| {
        format!(
            "{}:{}:{}: {}",
            opts.input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        )
    })?;
    let st = resolve_result.st;
    let type_layouts = resolve_result.type_layouts;

    // Build external globals from .amod-loaded modules.
    let mut external_globals = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_globals.extend(crate::sema::amod::extract_module_globals(ext_mod));
    }

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
    // Build external char_len_star_params from .amod-loaded modules.
    let mut external_char_len_star = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_char_len_star.extend(crate::sema::amod::extract_char_len_star_params(ext_mod));
    }

    let (mut ir_module, module_globals) = lower::lower_file(&units, &st, &type_layouts, external_globals, external_char_len_star);
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
                format!(
                    "integer(16) / i128 optimization at -{} is not yet supported; use --emit-ir to inspect the raw IR for now",
                    opts.opt_level.as_flag()
                )
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
    if let Some(user_func) = main_wrapper_target(&allocated) {
        if user_func != "main" {
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
                user_func
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
        // Emit .amod files for each MODULE in the compilation unit.
        for unit in &units {
            if let crate::ast::unit::ProgramUnit::Module { name, .. } = &unit.node {
                let mod_key = name.to_lowercase();
                if let Some(mod_scope_id) = st.find_module_scope(&mod_key) {
                    let amod_text = crate::sema::amod::write_amod(
                        name,
                        opts.input.to_str().unwrap_or(""),
                        &source,
                        &st,
                        mod_scope_id,
                        &module_globals,
                        &type_layouts,
                        &ir_module,
                        &std::collections::HashMap::new(), // char_len_star computed by writer from scope
                    );
                    let amod_path = opts.output_path()
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join(format!("{}.amod", mod_key));
                    if let Err(e) = fs::write(&amod_path, &amod_text) {
                        eprintln!("warning: cannot write {}: {}", amod_path.display(), e);
                    }
                }
            }
        }
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
    if let Some(workspace_root) = find_workspace_root() {
        maybe_refresh_runtime_lib(&workspace_root)?;
        for candidate in [
            workspace_root.join("target/debug/libarmfortas_rt.a"),
            workspace_root.join("target/release/libarmfortas_rt.a"),
        ] {
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }

    // Fall back to a runtime shipped next to the compiler binary.
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let candidate = dir.join("libarmfortas_rt.a");
        if candidate.exists() {
            return Ok(candidate.to_str().unwrap().to_string());
        }
    }

    Err("cannot find libarmfortas_rt.a — build with 'cargo build -p armfortas-rt'".into())
}

fn maybe_refresh_runtime_lib(workspace_root: &Path) -> Result<(), String> {
    let runtime_dir = workspace_root.join("runtime");
    if !runtime_dir.join("Cargo.toml").exists() {
        return Ok(());
    }

    let Some(source_mtime) = newest_mtime(&runtime_dir) else {
        return Ok(());
    };
    let debug_archive = workspace_root.join("target/debug/libarmfortas_rt.a");
    let archive_mtime = fs::metadata(&debug_archive).ok().and_then(|meta| meta.modified().ok());

    if archive_mtime.is_some_and(|mtime| mtime >= source_mtime) {
        return Ok(());
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root)
        .args(["build", "-p", "armfortas-rt"])
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

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            let child = newest_mtime(&entry.path())?;
            if child > newest {
                newest = child;
            }
        }
    }
    Some(newest)
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
            if ancestor.join("Cargo.toml").exists() && ancestor.join("runtime/Cargo.toml").exists() {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    None
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
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
            module_search_paths: vec![],
        };

        compile(&opts).expect("integer(16) multiply should codegen at O1 after const fold");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(asm.contains("movz x16, #42"), "expected folded i128 constant in asm:\n{}", asm);
        assert!(!asm.contains("mul "), "expected O1 i128 multiply to fold away before backend:\n{}", asm);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn main_wrapper_prefers_program_body_over_earlier_helpers() {
        let allocated = vec![
            MachineFunction::new("bump".into()),
            MachineFunction::new("__prog_audit_entry".into()),
        ];

        assert_eq!(
            main_wrapper_target(&allocated),
            Some("__prog_audit_entry"),
            "main wrapper should call the lowered program body, not the first helper"
        );
    }
}
