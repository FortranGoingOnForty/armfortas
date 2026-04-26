//! Compilation driver.
//!
//! CLI argument parsing, phase orchestration, multi-file compilation,
//! dependency resolution, and linker invocation.

pub mod defaults;
pub mod dep_scan;
pub mod diag;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::codegen::mir::MachineFunction;
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

/// Source-form override requested on the command line.  None means
/// detect from the file extension (.f90 → free, .f / .for → fixed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormOverride {
    Free,
    Fixed,
}

/// Action that should run when args parsing completes successfully
/// without producing a compile job (e.g. --help, --version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoAction {
    Help,
    Version,
    DumpVersion,
}

/// Result of parsing CLI args — either a real compile job or an
/// informational request.
pub enum ParsedCli {
    Compile(Box<Options>),
    Info(InfoAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliInputKind {
    FortranSource,
    LinkArtifact,
}

/// Compilation options.
pub struct Options {
    // ---- I/O ----
    pub input: PathBuf,
    /// Additional input files for multi-source mode.
    pub extra_inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,

    // ---- Mode ----
    pub emit_asm: bool,        // -S
    pub emit_obj: bool,        // -c
    pub emit_ir: bool,         // --emit-ir
    pub emit_ast: bool,        // --emit-ast
    pub emit_tokens: bool,     // --emit-tokens
    pub preprocess_only: bool, // -E
    pub preprocessor_defines: Vec<(String, String)>,
    pub cpp_compat: bool, // -cpp (accepted; preprocessing already runs)

    // ---- Language ----
    pub std: Option<crate::sema::validate::FortranStandard>,
    pub source_form_override: Option<SourceFormOverride>,
    pub default_integer_8: bool,
    pub default_real_8: bool,
    pub force_implicit_none: bool,
    pub recursive_default: bool,
    pub backslash_escapes: bool,
    pub free_line_length_none_compat: bool,
    pub max_stack_var_size: Option<u64>,

    // ---- Optimization ----
    pub opt_level: OptLevel,

    // ---- Warnings ----
    pub warn_all: bool,
    pub warn_extra: bool,
    pub warn_pedantic: bool,
    pub warn_deprecated: bool,
    pub warn_as_error: bool,
    pub disabled_warnings: Vec<String>,
    pub cli_warnings: Vec<String>,

    // ---- Debug / introspection ----
    pub debug_info: bool,                      // -g (accepted; DWARF deferred)
    pub verbose: bool,                         // -v
    pub time_report: bool,                     // --time-report
    pub diagnostics_format: DiagnosticsFormat, // --diagnostics-format=
    pub check_bounds: bool,                    // -fcheck=bounds
    pub check_all: bool,                       // -fcheck=all
    pub backtrace_requested: bool,             // -fbacktrace (accepted; runtime wiring TODO)

    // ---- Search paths / linking ----
    /// Directories to search for `.amod` module files (`-I <dir>`).
    pub module_search_paths: Vec<PathBuf>,
    /// Directory to write generated `.amod` files (`-J <dir>`).
    pub module_output_dir: Option<PathBuf>,
    /// `-L <dir>` library search paths passed to `ld`.
    pub library_search_paths: Vec<PathBuf>,
    /// `-l<name>` libraries passed to `ld`.
    pub link_libs: Vec<String>,
    /// `-shared` / `-static`.
    pub shared: bool,
    pub static_link: bool,
    /// `-rpath` entries passed to `ld`.
    pub rpath: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsFormat {
    Text,
    Json,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            extra_inputs: Vec::new(),
            output: None,
            emit_asm: false,
            emit_obj: false,
            emit_ir: false,
            emit_ast: false,
            emit_tokens: false,
            preprocess_only: false,
            preprocessor_defines: Vec::new(),
            cpp_compat: false,
            std: Some(crate::sema::validate::FortranStandard::F2018),
            source_form_override: None,
            default_integer_8: false,
            default_real_8: false,
            force_implicit_none: false,
            recursive_default: false,
            backslash_escapes: false,
            free_line_length_none_compat: false,
            max_stack_var_size: None,
            opt_level: OptLevel::O0,
            warn_all: false,
            warn_extra: false,
            warn_pedantic: false,
            warn_deprecated: false,
            warn_as_error: false,
            disabled_warnings: Vec::new(),
            cli_warnings: Vec::new(),
            debug_info: false,
            verbose: false,
            time_report: false,
            diagnostics_format: DiagnosticsFormat::Text,
            check_bounds: false,
            check_all: false,
            backtrace_requested: false,
            module_search_paths: Vec::new(),
            module_output_dir: None,
            library_search_paths: Vec::new(),
            link_libs: Vec::new(),
            shared: false,
            static_link: false,
            rpath: Vec::new(),
        }
    }
}

impl Options {
    /// Old name preserved for callers that haven't been migrated.
    /// New code should call `parse_cli` and dispatch on `ParsedCli`.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        match parse_cli(args)? {
            ParsedCli::Compile(opts) => Ok(*opts),
            ParsedCli::Info(_) => Err("info request — call parse_cli".into()),
        }
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
        } else if self.emit_ast {
            PathBuf::from(format!("{}.ast", stem))
        } else if self.emit_tokens {
            PathBuf::from(format!("{}.tokens", stem))
        } else {
            PathBuf::from(stem)
        }
    }
}

/// Parse the command line.  Returns either a compile job or a request
/// for informational output (so main.rs can branch and exit cleanly
/// without a compile attempt).  Supports response files via `@file`,
/// joined-form short options (`-Idir`, `-O2`, `-llib`), and
/// `--key=value` style for the long options that take a value.
pub fn parse_cli(raw_args: &[String]) -> Result<ParsedCli, String> {
    let args = expand_response_files(raw_args)?;
    let mut opts = Options::default();
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut info_action: Option<InfoAction> = None;
    let mut unknown_warning_flags = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].clone();
        match arg.as_str() {
            // ---- Information ----
            "--help" | "-h" => info_action = Some(InfoAction::Help),
            "--version" | "-V" => info_action = Some(InfoAction::Version),
            "-dumpversion" => info_action = Some(InfoAction::DumpVersion),

            // ---- Output path ----
            "-o" => {
                i += 1;
                let value = args.get(i).ok_or("-o requires an argument")?;
                set_output_path(&mut opts, value)?;
            }
            arg if arg.starts_with("-o") => {
                set_output_path(&mut opts, short_option_value(arg, "-o", "an argument")?)?;
            }

            // ---- Mode ----
            "-S" => opts.emit_asm = true,
            "-c" => opts.emit_obj = true,
            "-E" => opts.preprocess_only = true,
            "-cpp" => opts.cpp_compat = true,
            "-D" => {
                i += 1;
                let spec = args.get(i).ok_or("-D requires a macro name")?;
                opts.preprocessor_defines
                    .push(parse_preprocessor_define(spec)?);
            }
            arg if arg.starts_with("-D") => {
                opts.preprocessor_defines
                    .push(parse_preprocessor_define(&arg[2..])?);
            }
            "--emit-ir" => opts.emit_ir = true,
            "--emit-ast" => opts.emit_ast = true,
            "--emit-tokens" => opts.emit_tokens = true,

            // ---- Optimization ----
            "-O" => opts.opt_level = OptLevel::O0,
            arg if arg.starts_with("-O") => {
                opts.opt_level = OptLevel::parse_flag(&arg[1..])
                    .ok_or_else(|| format!("unknown optimization level: {}", arg))?;
            }

            // ---- Module / include search paths ----
            "-I" => {
                i += 1;
                opts.module_search_paths
                    .push(PathBuf::from(args.get(i).ok_or("-I requires a directory")?));
            }
            arg if arg.starts_with("-I") => opts
                .module_search_paths
                .push(PathBuf::from(short_option_value(arg, "-I", "a directory")?)),

            "-J" => {
                i += 1;
                let dir = PathBuf::from(args.get(i).ok_or("-J requires a directory")?);
                if !opts.module_search_paths.iter().any(|path| path == &dir) {
                    opts.module_search_paths.push(dir.clone());
                }
                opts.module_output_dir = Some(dir);
            }
            "-module" => {
                i += 1;
                let dir = PathBuf::from(args.get(i).ok_or("-module requires a directory")?);
                if !opts.module_search_paths.iter().any(|path| path == &dir) {
                    opts.module_search_paths.push(dir.clone());
                }
                opts.module_output_dir = Some(dir);
            }
            arg if arg.starts_with("-J") => {
                let dir = PathBuf::from(short_option_value(arg, "-J", "a directory")?);
                if !opts.module_search_paths.iter().any(|path| path == &dir) {
                    opts.module_search_paths.push(dir.clone());
                }
                opts.module_output_dir = Some(dir);
            }

            // ---- Linker search / libs / rpath ----
            "-L" => {
                i += 1;
                opts.library_search_paths
                    .push(PathBuf::from(args.get(i).ok_or("-L requires a directory")?));
            }
            arg if arg.starts_with("-L") => opts
                .library_search_paths
                .push(PathBuf::from(short_option_value(arg, "-L", "a directory")?)),

            "-l" => {
                i += 1;
                opts.link_libs
                    .push(args.get(i).ok_or("-l requires a library name")?.clone());
            }
            arg if arg.starts_with("-l") => opts
                .link_libs
                .push(short_option_value(arg, "-l", "a library name")?.to_string()),

            "-rpath" | "--rpath" => {
                i += 1;
                opts.rpath
                    .push(PathBuf::from(args.get(i).ok_or("-rpath requires a path")?));
            }

            "-shared" => opts.shared = true,
            "-static" => opts.static_link = true,

            // ---- Standards / language flags ----
            arg if arg.starts_with("-std=") => {
                let val = &arg["-std=".len()..];
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown -std value: {}", val))?,
                );
            }
            "-std" => {
                i += 1;
                let val = args.get(i).ok_or("-std requires a value")?;
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown -std value: {}", val))?,
                );
            }
            arg if arg.starts_with("--std=") => {
                let val = &arg["--std=".len()..];
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown --std value: {}", val))?,
                );
            }
            "--std" => {
                i += 1;
                let val = args.get(i).ok_or("--std requires a value")?;
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown --std value: {}", val))?,
                );
            }
            "-ffree-form" => opts.source_form_override = Some(SourceFormOverride::Free),
            "-ffixed-form" => opts.source_form_override = Some(SourceFormOverride::Fixed),
            "-ffree-line-length-none" => opts.free_line_length_none_compat = true,
            "-fdefault-integer-8" => opts.default_integer_8 = true,
            "-fdefault-real-8" => opts.default_real_8 = true,
            "-fimplicit-none" => opts.force_implicit_none = true,
            "-frecursive" => opts.recursive_default = true,
            "-fbackslash" => opts.backslash_escapes = true,
            "-fno-backslash" => opts.backslash_escapes = false,
            arg if arg.starts_with("-fmax-stack-var-size=") => {
                let val = &arg["-fmax-stack-var-size=".len()..];
                opts.max_stack_var_size = Some(
                    val.parse()
                        .map_err(|_| format!("invalid -fmax-stack-var-size value: {}", val))?,
                );
            }

            // ---- Runtime checks ----
            "-fcheck=bounds" => opts.check_bounds = true,
            "-fcheck=all" => {
                opts.check_bounds = true;
                opts.check_all = true;
            }
            "-fbacktrace" => opts.backtrace_requested = true,

            // ---- Warnings (accepted; gating is gradual sprint work) ----
            "-Wall" => opts.warn_all = true,
            "-Wextra" => opts.warn_extra = true,
            "-Wpedantic" | "-pedantic" => opts.warn_pedantic = true,
            "-Wdeprecated" => opts.warn_deprecated = true,
            "-Werror" => opts.warn_as_error = true,
            arg if arg.starts_with("-Wno-") => {
                opts.disabled_warnings.push(arg[5..].to_string());
            }
            arg if arg.starts_with("-W") => {
                unknown_warning_flags.push(arg.to_string());
            }

            // ---- Debug / introspection ----
            "-g" | "-g1" | "-g2" | "-g3" | "-g0" => opts.debug_info = true,
            arg if arg.starts_with("-g") => opts.debug_info = true,
            "-v" | "--verbose" => opts.verbose = true,
            "--time-report" => opts.time_report = true,
            arg if arg.starts_with("--diagnostics-format=") => {
                let val = &arg["--diagnostics-format=".len()..];
                opts.diagnostics_format = match val {
                    "text" => DiagnosticsFormat::Text,
                    "json" => DiagnosticsFormat::Json,
                    other => return Err(format!("unknown --diagnostics-format value: {}", other)),
                };
            }

            // ---- Positional input file ----
            arg if !arg.starts_with('-') => inputs.push(PathBuf::from(arg)),

            other => return Err(format!("unknown option: {}", other)),
        }
        i += 1;
    }

    if let Some(action) = info_action {
        return Ok(ParsedCli::Info(action));
    }

    if opts.shared && opts.static_link {
        return Err("-shared and -static are mutually exclusive".into());
    }

    collect_cli_warnings(&mut opts, &unknown_warning_flags);

    if matches!(opts.diagnostics_format, DiagnosticsFormat::Json) {
        return Err("JSON diagnostics are not yet implemented".into());
    }

    if inputs.is_empty() {
        return Err("no input file".into());
    }
    opts.input = inputs.remove(0);
    opts.extra_inputs = inputs;
    Ok(ParsedCli::Compile(Box::new(opts)))
}

fn parse_preprocessor_define(spec: &str) -> Result<(String, String), String> {
    if spec.is_empty() {
        return Err("-D requires a macro name".into());
    }
    let (name, value) = match spec.split_once('=') {
        Some((name, value)) => (name, value),
        None => (spec, "1"),
    };
    if name.is_empty() {
        return Err(format!(
            "invalid macro definition '{}': missing macro name",
            spec
        ));
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(format!(
            "invalid macro definition '{}': missing macro name",
            spec
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(format!(
            "invalid macro definition '{}': macro name must start with a letter or underscore",
            spec
        ));
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err(format!(
            "invalid macro definition '{}': macro name must be alphanumeric or underscore",
            spec
        ));
    }
    Ok((name.to_string(), value.to_string()))
}

/// Expand any `@file` argument into the lines of `file`, treating
/// each whitespace-separated token as an additional argument.
fn expand_response_files(args: &[String]) -> Result<Vec<String>, String> {
    let mut expanded: Vec<String> = Vec::with_capacity(args.len());
    let mut stack = Vec::new();
    for arg in args {
        expand_response_arg(arg, None, 0, &mut stack, &mut expanded)?;
    }
    Ok(expanded)
}

fn expand_response_arg(
    arg: &str,
    base_dir: Option<&Path>,
    depth: usize,
    stack: &mut Vec<PathBuf>,
    expanded: &mut Vec<String>,
) -> Result<(), String> {
    const RESPONSE_FILE_DEPTH_LIMIT: usize = 8;

    if let Some(literal) = arg.strip_prefix("@@") {
        expanded.push(format!("@{}", literal));
        return Ok(());
    }

    let Some(path) = arg.strip_prefix('@') else {
        expanded.push(arg.to_string());
        return Ok(());
    };

    if depth >= RESPONSE_FILE_DEPTH_LIMIT {
        return Err(format!(
            "response file nesting exceeds limit of {}",
            RESPONSE_FILE_DEPTH_LIMIT
        ));
    }

    let resolved = resolve_response_file_path(path, base_dir);
    let display = resolved.display().to_string();
    let canonical = fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
    if stack.contains(&canonical) {
        return Err(format!("circular response file '{}'", display));
    }

    let body = fs::read_to_string(&resolved)
        .map_err(|e| format!("cannot read response file '{}': {}", display, e))?;
    let tokens = parse_response_file_tokens(&body)
        .map_err(|e| format!("cannot parse response file '{}': {}", display, e))?;
    let next_base = resolved.parent().map(Path::to_path_buf);

    stack.push(canonical);
    for token in tokens {
        expand_response_arg(&token, next_base.as_deref(), depth + 1, stack, expanded)?;
    }
    stack.pop();
    Ok(())
}

fn resolve_response_file_path(path: &str, base_dir: Option<&Path>) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else if let Some(base_dir) = base_dir {
        base_dir.join(candidate)
    } else {
        candidate
    }
}

fn parse_response_file_tokens(body: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = body.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some(delim) => match ch {
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        current.push('\\');
                    }
                }
                c if c == delim => quote = None,
                _ => current.push(ch),
            },
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        current.push('\\');
                    }
                }
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if let Some(delim) = quote {
        return Err(format!("unterminated {} quote", delim));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn short_option_value<'a>(arg: &'a str, flag: &str, what: &str) -> Result<&'a str, String> {
    let tail = &arg[flag.len()..];
    let value = tail.strip_prefix('=').unwrap_or(tail);
    if value.is_empty() {
        Err(format!("{} requires {}", flag, what))
    } else {
        Ok(value)
    }
}

fn set_output_path(opts: &mut Options, value: &str) -> Result<(), String> {
    if opts.output.is_some() {
        return Err("duplicate -o: output path already specified".into());
    }
    if value.is_empty() {
        return Err("-o requires an argument".into());
    }
    opts.output = Some(PathBuf::from(value));
    Ok(())
}

fn collect_cli_warnings(opts: &mut Options, unknown_warning_flags: &[String]) {
    if opts.cpp_compat {
        opts.cli_warnings.push(
            "-cpp is accepted for compatibility; preprocessing already runs for Fortran inputs"
                .into(),
        );
    }

    if opts.check_all {
        opts.cli_warnings.push(
            "-fcheck=all is accepted, but only array bounds checks exist today and those are already always enabled".into(),
        );
    } else if opts.check_bounds {
        opts.cli_warnings.push(
            "-fcheck=bounds currently has no effect because array bounds checks are already always enabled".into(),
        );
    }

    if opts.max_stack_var_size.is_some() {
        opts.cli_warnings
            .push("-fmax-stack-var-size is recognized but not yet implemented".into());
    }
    if opts.free_line_length_none_compat {
        opts.cli_warnings.push(
            "-ffree-line-length-none is accepted for compatibility; free-form inputs already have no line-length limit".into(),
        );
    }
    if opts.recursive_default {
        opts.cli_warnings
            .push("-frecursive is recognized but not yet implemented".into());
    }
    if opts.backslash_escapes {
        opts.cli_warnings.push(
            "-fbackslash is recognized but string escape processing is not yet implemented".into(),
        );
    }

    if opts.warn_all {
        opts.cli_warnings
            .push("-Wall is recognized but warning-group emission is not yet implemented".into());
    }
    if opts.warn_extra {
        opts.cli_warnings
            .push("-Wextra is recognized but warning-group emission is not yet implemented".into());
    }

    if opts.debug_info {
        opts.cli_warnings
            .push("-g is accepted, but debug info emission is not yet implemented".into());
    }
    if opts.backtrace_requested {
        opts.cli_warnings.push(
            "-fbacktrace is accepted, but runtime backtrace control is not yet implemented".into(),
        );
    }

    let suppress_unknown_warning_option = opts
        .disabled_warnings
        .iter()
        .any(|name| name == "unknown-warning-option");
    if !suppress_unknown_warning_option {
        for flag in unknown_warning_flags {
            opts.cli_warnings
                .push(format!("unrecognized warning option '{}'", flag));
        }
    }
}

/// Help text printed by `--help`.
pub const HELP_TEXT: &str = "\
USAGE: armfortas [OPTIONS] <files...>
       afs [OPTIONS] <files...>

COMPILATION:
  -c                          Compile to object file only (no linking)
  -S                          Emit assembly text
  -E                          Preprocess only
  -cpp                        Accept GNU-style preprocessing flag
  -D<name>[=<value>]          Define a preprocessor macro
  -o <file>                   Output file name

LANGUAGE:
  -std=<standard>             GNU-compatible alias for --std=<standard>
  --std=<standard>            Fortran standard (f77, f90, f95, f2003, f2008, f2018, f2023)
  -ffree-form                 Force free-form source
  -ffixed-form                Force fixed-form source
  -ffree-line-length-none     GNU-compatible alias; free-form inputs are already unlimited
  -fdefault-integer-8         Make default integer kind 8 bytes
  -fdefault-real-8            Make default real kind 8 bytes
  -fimplicit-none             Force implicit none in all scopes
  -frecursive                 Make all procedures recursive by default
  -fbackslash                 Interpret backslash in strings as escape
  -fmax-stack-var-size=<n>    Stack variable size threshold (bytes)

OPTIMIZATION:
  -O0, -O1, -O2, -O3          Optimization level (default -O0)
  -Os                         Optimize for size
  -Ofast                      Aggressive optimization

WARNINGS:
  -Wall                       All standard warnings
  -Wextra                     Extra warnings
  -Wpedantic                  Pedantic standard conformance warnings
  -Wdeprecated                Deprecated feature warnings
  -Werror                     Treat warnings as errors
  -Wno-<name>                 Disable specific warning

DEBUGGING:
  -g                          Generate debug information (DWARF emission TODO)
  -fbacktrace                 Accept GNU-style runtime backtrace flag
  --emit-ir                   Dump IR to the output path
  --emit-ast                  Dump AST to the output path
  --emit-tokens               Dump token stream to the output path
  -v, --verbose               Verbose output (show compilation phases)
  --time-report               Show time spent in each compilation phase
  -fcheck=bounds              Enable runtime array bounds checking
  -fcheck=all                 Enable all runtime checks
  --diagnostics-format=text|json
                              Diagnostic output format

DIRECTORIES:
  -I <dir>                    Module/include search path
  -J <dir>                    Module output directory
  -L <dir>                    Library search path
  -l <lib>                    Link library

LINKING:
  -shared                     Produce shared library
  -static                     Static linking
  -rpath <path>               Runtime library path

INFORMATION:
  --version, -V               Print version
  --help, -h                  Print help
  -dumpversion                Print version number only
                              If multiple info flags are given, the last one wins

OTHER:
  @<file>                     Read additional arguments from <file> (one per token)
  @@<arg>                     Pass a literal argument beginning with @
";

pub fn program_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg| {
            Path::new(&arg)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .or_else(|| {
            std::env::current_exe().ok().and_then(|path| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "armfortas".into())
}

/// Version string emitted by `--version`.
pub fn version_string() -> String {
    format!(
        "{} {} (aarch64-apple-darwin)",
        program_name(),
        env!("CARGO_PKG_VERSION")
    )
}

/// Just the version number, for `-dumpversion`.
pub fn dump_version_string() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Tracks per-phase wall-clock time for `--time-report`.  When
/// disabled, all operations are zero-overhead (no Instant calls, no
/// allocation).
struct PhaseTimer {
    enabled: bool,
    samples: Vec<(&'static str, std::time::Duration)>,
    start: Option<std::time::Instant>,
}

struct PhaseGuard {
    name: &'static str,
    started: Option<std::time::Instant>,
}

impl PhaseTimer {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            samples: Vec::new(),
            start: if enabled {
                Some(std::time::Instant::now())
            } else {
                None
            },
        }
    }
    fn start(&self, name: &'static str) -> PhaseGuard {
        PhaseGuard {
            name,
            started: if self.enabled {
                Some(std::time::Instant::now())
            } else {
                None
            },
        }
    }
    fn record(&mut self, name: &'static str, dur: std::time::Duration) {
        if self.enabled {
            self.samples.push((name, dur));
        }
    }
    fn report(&self) {
        if !self.enabled {
            return;
        }
        let total: std::time::Duration = self
            .samples
            .iter()
            .map(|(_, d)| *d)
            .sum::<std::time::Duration>();
        let total_ms = total.as_secs_f64() * 1000.0;
        eprintln!("Phase            Time (ms)    %");
        eprintln!("─────────────────────────────────");
        for (name, d) in &self.samples {
            let ms = d.as_secs_f64() * 1000.0;
            let pct = if total_ms > 0.0 {
                ms / total_ms * 100.0
            } else {
                0.0
            };
            eprintln!("{:<16} {:>8.2} {:>4.0}%", name, ms, pct);
        }
        eprintln!("─────────────────────────────────");
        let wall = self
            .start
            .map(|s| s.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        eprintln!("{:<16} {:>8.2} {:>4.0}%", "Total", wall, 100.0);
    }
}

impl PhaseGuard {
    fn end(self, timer: &mut PhaseTimer) {
        if let Some(start) = self.started {
            timer.record(self.name, start.elapsed());
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

fn all_input_paths(opts: &Options) -> Vec<PathBuf> {
    let mut inputs = vec![opts.input.clone()];
    inputs.extend(opts.extra_inputs.iter().cloned());
    inputs
}

fn classify_cli_input(path: &Path) -> CliInputKind {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("o" | "obj" | "a" | "dylib" | "so") => CliInputKind::LinkArtifact,
        _ => CliInputKind::FortranSource,
    }
}

fn validate_link_only_inputs(opts: &Options) -> Result<(), String> {
    if opts.preprocess_only {
        return Err("-E cannot be used when all inputs are prebuilt objects or archives".into());
    }
    if opts.emit_tokens {
        return Err(
            "--emit-tokens cannot be used when all inputs are prebuilt objects or archives".into(),
        );
    }
    if opts.emit_ast {
        return Err(
            "--emit-ast cannot be used when all inputs are prebuilt objects or archives".into(),
        );
    }
    if opts.emit_ir {
        return Err(
            "--emit-ir cannot be used when all inputs are prebuilt objects or archives".into(),
        );
    }
    if opts.emit_asm {
        return Err("-S cannot be used when all inputs are prebuilt objects or archives".into());
    }
    if opts.emit_obj {
        return Err("-c cannot be used when all inputs are prebuilt objects or archives".into());
    }
    Ok(())
}

/// Execute a fully parsed CLI job, dispatching between source
/// compilation and pure link steps based on the positional inputs.
pub fn execute(opts: &Options) -> Result<(), String> {
    let inputs = all_input_paths(opts);
    let has_source = inputs
        .iter()
        .any(|path| classify_cli_input(path) == CliInputKind::FortranSource);
    let has_link_artifact = inputs
        .iter()
        .any(|path| classify_cli_input(path) == CliInputKind::LinkArtifact);

    match (has_source, has_link_artifact) {
        (true, false) => {
            if opts.extra_inputs.is_empty() {
                compile(opts)
            } else {
                compile_multi(opts)
            }
        }
        (false, true) => {
            validate_link_only_inputs(opts)?;
            let output = opts.output.clone().unwrap_or_else(|| PathBuf::from("a.out"));
            link_inputs(&inputs, &output, opts)
        }
        (true, true) => Err(
            "mixing Fortran sources with prebuilt object/archive inputs is not yet supported; compile the sources first and then link the resulting objects".into(),
        ),
        (false, false) => unreachable!("parse_cli guarantees at least one input"),
    }
}

/// Compile a Fortran source file through the full pipeline.
pub fn compile(opts: &Options) -> Result<(), String> {
    let mut phases = PhaseTimer::new(opts.time_report);
    if opts.verbose {
        eprintln!("{}", version_string());
    }

    // Reset / install the default-kind globals so subsequent passes
    // see this run's -fdefault-{integer,real}-8 settings cleanly,
    // even when multiple compile() calls share a process (cargo test).
    defaults::reset();
    if opts.default_integer_8 {
        defaults::set_default_int_kind(8);
    }
    if opts.default_real_8 {
        defaults::set_default_real_kind(8);
    }

    // 1. Read source.
    if opts.verbose {
        eprintln!(" reading: {}", opts.input.display());
    }
    let phase = phases.start("read");
    // Fortran source files in the wild are not always valid UTF-8 — Latin-1
    // and stray bytes appear in comments or string literals.  gfortran/flang
    // both accept non-UTF-8 sources; mirror that by reading raw bytes and
    // decoding lossily so invalid sequences become U+FFFD instead of an I/O
    // failure.
    let raw = fs::read(&opts.input)
        .map_err(|e| format!("cannot read '{}': {}", opts.input.display(), e))?;
    let source = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    phase.end(&mut phases);
    let file_str = opts.input.display().to_string();

    // 2. Preprocess.
    let source_form = match opts.source_form_override {
        Some(SourceFormOverride::Free) => SourceForm::FreeForm,
        Some(SourceFormOverride::Fixed) => SourceForm::FixedForm,
        None => detect_source_form(&opts.input.to_string_lossy()),
    };
    if opts.std == Some(crate::sema::validate::FortranStandard::F77)
        && matches!(source_form, SourceForm::FreeForm)
    {
        return Err(format!(
            "{}: --std=f77 requires fixed-form source (.f/.for or -ffixed-form)",
            opts.input.display()
        ));
    }
    if opts.verbose {
        let form = match source_form {
            SourceForm::FreeForm => "free-form",
            SourceForm::FixedForm => "fixed-form",
        };
        eprintln!(" preprocessing: {} ({})", opts.input.display(), form);
    }
    let phase = phases.start("preprocess");
    let mut pp_config = crate::preprocess::PreprocConfig {
        filename: opts.input.to_str().unwrap_or("<input>").to_string(),
        fixed_form: matches!(source_form, SourceForm::FixedForm),
        // Share `-I` paths with the preprocessor so `#include "foo.inc"`
        // can find headers (e.g. stdlib's `include/macros.inc`).  The
        // resolver searches relative-to-current-file first, then this
        // list — both gfortran and flang do the same.
        include_paths: opts.module_search_paths.clone(),
        ..crate::preprocess::PreprocConfig::default()
    };
    for (name, value) in &opts.preprocessor_defines {
        pp_config
            .defines
            .insert(name.clone(), crate::preprocess::MacroDef::object(value));
    }
    let pp_result =
        crate::preprocess::preprocess(&source, &pp_config).map_err(|e| format!("{}", e))?;
    phase.end(&mut phases);
    let preprocessed = pp_result.text;

    if opts.preprocess_only {
        if opts.output.is_none() {
            print!("{}", preprocessed);
        } else {
            let out = opts.output_path();
            if out.as_os_str() == "-" {
                print!("{}", preprocessed);
            } else {
                fs::write(&out, &preprocessed)
                    .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
            }
            if opts.verbose {
                eprintln!(" preprocess-only: wrote {}", out.display());
            }
            phases.report();
            return Ok(());
        }
        if opts.verbose {
            eprintln!(" preprocess-only: wrote stdout");
        }
        phases.report();
        return Ok(());
    }

    // 3. Lex.
    let phase = phases.start("lex");
    let tokens = match tokenize(&preprocessed, 0, source_form) {
        Ok(tokens) => tokens,
        Err(e) => {
            phase.end(&mut phases);
            diag::render(
                &file_str,
                &source,
                e.span,
                diag::Level::Error,
                &format!("lexer error: {}", e.msg),
                1,
            );
            phases.report();
            return Err(format!(
                "aborting due to errors in {}",
                opts.input.display()
            ));
        }
    };
    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" lexed: {} tokens", tokens.len());
    }
    if opts.emit_tokens {
        let out = opts.output_path();
        let mut buf = String::new();
        for t in &tokens {
            buf.push_str(&format!("{:?}\n", t));
        }
        fs::write(&out, &buf).map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        return Ok(());
    }

    // 4. Parse.
    let phase = phases.start("parse");
    let mut parser = Parser::new(&tokens);
    let units = match parser.parse_file() {
        Ok(units) => units,
        Err(e) => {
            phase.end(&mut phases);
            let span_len =
                if e.span.end.line == e.span.start.line && e.span.end.col > e.span.start.col {
                    (e.span.end.col - e.span.start.col) as usize
                } else {
                    1
                };
            diag::render(
                &file_str,
                &source,
                e.span,
                diag::Level::Error,
                &format!("parse error: {}", e.msg),
                span_len,
            );
            phases.report();
            return Err(format!(
                "aborting due to errors in {}",
                opts.input.display()
            ));
        }
    };
    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" parsed: {} top-level units", units.len());
    }
    if opts.emit_ast {
        let out = opts.output_path();
        let mut buf = String::new();
        for u in &units {
            buf.push_str(&format!("{:#?}\n", u));
        }
        fs::write(&out, &buf).map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        return Ok(());
    }

    // 5. Semantic analysis.
    let phase = phases.start("sema");
    let resolve_result = resolve::resolve_file(&units, &opts.module_search_paths).map_err(|e| {
        format!(
            "{}:{}:{}: {}",
            opts.input.display(),
            e.span.start.line,
            e.span.start.col,
            e.msg
        )
    })?;
    let mut st = resolve_result.st;
    if opts.force_implicit_none {
        st.force_implicit_none_all_units();
    }
    let type_layouts = resolve_result.type_layouts;

    // Build external globals from .amod-loaded modules.
    let mut external_globals = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_globals.extend(crate::sema::amod::extract_module_globals(ext_mod));
    }

    let diags = validate::validate_file_with_layouts_and_warning_groups(
        &units,
        &st,
        opts.std,
        &type_layouts,
        opts.warn_pedantic,
        opts.warn_deprecated,
    );
    phase.end(&mut phases);
    let mut had_error = false;
    for d in &diags {
        let level = match d.kind {
            validate::DiagKind::Error => diag::Level::Error,
            validate::DiagKind::Warning => diag::Level::Warning,
        };
        // span_len is best-effort: end.col >= start.col on the same
        // line gives a nice underline, otherwise default to 1.
        let span_len = if d.span.end.line == d.span.start.line && d.span.end.col > d.span.start.col
        {
            (d.span.end.col - d.span.start.col) as usize
        } else {
            1
        };
        diag::render(&file_str, &source, d.span, level, &d.msg, span_len);
        match d.kind {
            validate::DiagKind::Error => had_error = true,
            validate::DiagKind::Warning if opts.warn_as_error => had_error = true,
            _ => {}
        }
    }
    if had_error {
        phases.report();
        return Err(format!(
            "aborting due to errors in {}",
            opts.input.display()
        ));
    }
    if opts.verbose {
        eprintln!(" sema: {} diagnostics", diags.len());
    }

    // 6. Lower to IR.
    let mut external_optional_params = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_optional_params.extend(crate::sema::amod::extract_optional_params(ext_mod));
    }

    let mut external_descriptor_params = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_descriptor_params.extend(crate::sema::amod::extract_descriptor_params(ext_mod));
    }

    // Build external char_len_star_params from .amod-loaded modules.
    let mut external_char_len_star = std::collections::HashMap::new();
    for ext_mod in &resolve_result.external_modules {
        external_char_len_star.extend(crate::sema::amod::extract_char_len_star_params(ext_mod));
    }

    let (mut ir_module, module_globals) = lower::lower_file(
        &units,
        &st,
        &type_layouts,
        external_globals,
        external_optional_params,
        external_descriptor_params,
        external_char_len_star,
    );
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
    if opts.verbose {
        eprintln!(" IR: {} functions", ir_module.functions.len());
    }
    // 6.5. Run IR optimization pipeline.
    //
    // This is where const_fold, mem2reg, LICM, DSE, loop unrolling, and
    // every other IR-level pass actually fire. At O0 the pipeline is empty
    // so nothing changes. The pipeline runs to fixpoint; the pass manager
    // verifies the IR after every pass.
    let phase = phases.start("opt");
    {
        use crate::opt::pipeline::OptLevel as IrOpt;
        let ir_opt = match opts.opt_level {
            OptLevel::O0 => IrOpt::O0,
            OptLevel::O1 => IrOpt::O1,
            OptLevel::O2 => IrOpt::O2,
            OptLevel::O3 => IrOpt::O3,
            OptLevel::Os => IrOpt::Os,
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
    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" optimization: -{}", opts.opt_level.as_str());
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
            "backend does not yet support integer(16) / i128 codegen; use --emit-ir for now".into(),
        );
    }

    // 7. Instruction selection.
    let phase = phases.start("codegen");
    let machine_funcs = isel::select_module(&ir_module);

    // 7.5. Backend peephole (O2+): FMA fusion, etc.
    let mut allocated: Vec<_> = machine_funcs;
    if opts.opt_level >= OptLevel::O2 {
        for mf in &mut allocated {
            peephole::run_peephole(mf);
        }
    }

    let use_naive_regalloc = std::env::var_os("ARMFORTAS_USE_NAIVE_REGALLOC").is_some();

    // 8. Register allocation.
    for mf in &mut allocated {
        if use_naive_regalloc {
            crate::codegen::regalloc::regalloc_naive(mf);
        } else {
            let liveness = crate::codegen::liveness::compute_liveness(mf);
            let result = linearscan::linear_scan(mf);
            linearscan::apply_allocation(mf, &result, &liveness);
            linearscan::parallelize_call_arg_moves(mf);
            linearscan::insert_callee_saves(mf, &result.callee_saved_used);
            linearscan::coalesce_moves(mf);
            // 8.5. Tail call optimization (O1+): BL + epilogue → epilogue + B.
            // Runs after regalloc so we can inspect physical register assignments.
            if opts.opt_level >= OptLevel::O1 {
                crate::codegen::tailcall::tail_call_opt(mf);
            }
            // 8.6. Branch relaxation: any B.cond whose target lies
            // outside the ±1MB conditional-branch window is expanded
            // to a `B.{!cond} skip; B far_target; skip:` trampoline
            // so the assembler doesn't choke on the encoding.
            crate::codegen::relax_branches::relax_branches(mf);
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

    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" codegen: {} machine functions", allocated.len());
    }
    if opts.emit_asm {
        let out = opts.output_path();
        fs::write(&out, &asm_text)
            .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        if opts.verbose {
            eprintln!(" wrote: {}", out.display());
        }
        phases.report();
        return Ok(());
    }

    // 10. Assemble (using system assembler for now).
    //
    // The temp .s / .o paths must satisfy two competing needs:
    //   (1) `ld` embeds the .o path in the linked binary's symbol
    //       table (the OSO debug stab), so two back-to-back compiles
    //       of the same source to the same output path must use the
    //       same .o name — otherwise the embedded string varies and
    //       reproducible-build tests fail.  PID is unsafe here
    //       because each compile_binary call spawns a fresh
    //       subprocess with a different PID.
    //   (2) Two parallel compiles of two DIFFERENT sources with the
    //       same basename (e.g. both writing `mod.o` to different
    //       unique-dir test outputs) must NOT race on the same temp
    //       file.  Output stem alone is therefore not enough.
    // The cheap fix that satisfies both: derive a stable hash of the
    // full output path with FNV-1a and use it in the temp basename.
    // Same output path → same hash → same .o (deterministic across
    // subprocesses).  Different output paths → different hashes → no
    // collision.  std::collections::hash_map::DefaultHasher uses a
    // per-process random seed and would defeat (1).
    let out_path = opts.output_path();
    let stem = out_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "afs".to_string());
    let token = {
        let bytes = out_path.as_os_str().as_encoded_bytes();
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    };
    let asm_path = std::env::temp_dir().join(format!("armfortas_{}_{:016x}.s", stem, token));
    let obj_path = if opts.emit_obj {
        out_path.clone()
    } else {
        std::env::temp_dir().join(format!("armfortas_{}_{:016x}.o", stem, token))
    };

    fs::write(&asm_path, &asm_text).map_err(|e| format!("cannot write temp assembly: {}", e))?;

    let phase = phases.start("assemble");
    let as_result = if let Some(assembler) = env_override("AFS_AS_PATH") {
        Command::new(assembler)
            .arg(&asm_path)
            .arg("-o")
            .arg(&obj_path)
            .output()
            .map_err(|e| format!("cannot run assembler: {}", e))?
    } else {
        Command::new("as")
            .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("cannot run assembler: {}", e))?
    };
    phase.end(&mut phases);

    if !as_result.status.success() {
        let stderr = String::from_utf8_lossy(&as_result.stderr);
        return Err(format!("assembler failed:\n{}", stderr));
    }
    if opts.verbose {
        eprintln!(" assembled: {}", obj_path.display());
    }

    let local_descriptor_params = crate::ir::lower::collect_descriptor_params_for_units(&units);
    let local_char_len_star_params =
        crate::ir::lower::collect_char_len_star_params_for_units(&units);

    // Emit .amod files for each MODULE in the compilation unit.
    // -J <dir> overrides where they go. For compile-only (-c) builds
    // without -J, keep the traditional compiler behavior of writing
    // module files into the current working directory even if the
    // object output path points into a source subdirectory. For full
    // link/shared outputs, keep following the primary output path.
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
                    &local_descriptor_params,
                    &local_char_len_star_params,
                );
                let amod_dir: std::path::PathBuf =
                    opts.module_output_dir.clone().unwrap_or_else(|| {
                        if opts.emit_obj {
                            std::env::current_dir()
                                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                        } else {
                            opts.output_path()
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .to_path_buf()
                        }
                    });
                let amod_path = amod_dir.join(format!("{}.amod", mod_key));
                fs::write(&amod_path, &amod_text)
                    .map_err(|e| format!("cannot write '{}': {}", amod_path.display(), e))?;
                if opts.verbose {
                    eprintln!(" amod: {}", amod_path.display());
                }
            }
        }
    }

    if opts.emit_obj {
        phases.report();
        return Ok(());
    }

    // 11. Link.
    let binary_path = opts.output_path();
    let phase = phases.start("link");
    link(&obj_path, &binary_path, opts)?;
    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" linked: {}", binary_path.display());
    }

    // Cleanup.
    let _ = fs::remove_file(&asm_path);
    let _ = fs::remove_file(&obj_path);

    phases.report();
    Ok(())
}

/// Link an object file with the runtime library to produce a binary.
/// `opts` contributes the user-supplied `-L`, `-l`, `-rpath`,
/// `-shared`, and `-static` flags that need to make it through to ld.
fn link(obj: &Path, output: &Path, opts: &Options) -> Result<(), String> {
    link_inputs(&[obj.to_path_buf()], output, opts)
}

/// Link prebuilt objects and archives with the runtime to produce a
/// binary or shared library, preserving the user-supplied input order.
fn link_inputs(inputs: &[PathBuf], output: &Path, opts: &Options) -> Result<(), String> {
    if let Some(linker) = env_override("AFS_LD_PATH") {
        return link_inputs_with_afs_ld(&linker, inputs, output, opts);
    }

    let rt_path = find_runtime_lib()?;
    let sdk = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .map_err(|e| format!("cannot run xcrun: {}", e))?;
    let sysroot = String::from_utf8_lossy(&sdk.stdout).trim().to_string();

    let mut args: Vec<String> = vec!["-o".into(), output.to_string_lossy().into_owned()];
    for input in inputs {
        args.push(input.to_string_lossy().into_owned());
    }
    args.extend([
        rt_path,
        "-lSystem".into(),
        "-syslibroot".into(),
        sysroot,
        "-e".into(),
        "_main".into(),
    ]);
    push_link_flags(&mut args, opts);

    let ld_result = Command::new("ld")
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run linker: {}", e))?;

    if !ld_result.status.success() {
        let stderr = String::from_utf8_lossy(&ld_result.stderr);
        return Err(format!("linker failed:\n{}", stderr));
    }

    Ok(())
}

fn link_inputs_with_afs_ld(
    linker: &str,
    inputs: &[PathBuf],
    output: &Path,
    opts: &Options,
) -> Result<(), String> {
    if opts.shared {
        return Err("AFS_LD_PATH override does not yet support shared-library links".into());
    }
    if !opts.library_search_paths.is_empty() {
        return Err("AFS_LD_PATH override does not yet support -L search paths".into());
    }
    if !opts.link_libs.is_empty() {
        return Err("AFS_LD_PATH override does not yet support -l linker inputs".into());
    }
    if !opts.rpath.is_empty() {
        return Err("AFS_LD_PATH override does not yet support -rpath".into());
    }
    if opts.static_link {
        return Err("AFS_LD_PATH override does not yet support static-link mode".into());
    }

    let rt_path = find_runtime_lib()?;
    let libsystem_tbd = find_libsystem_tbd()?;
    let mut args: Vec<String> = vec![
        "-arch".into(),
        "arm64".into(),
        "-e".into(),
        "_main".into(),
        "-o".into(),
        output.to_string_lossy().into_owned(),
    ];
    for input in inputs {
        args.push(input.to_string_lossy().into_owned());
    }
    args.push(rt_path);
    args.push(libsystem_tbd);

    let output = Command::new(linker)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run linker: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "linker failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Append the user-supplied linker flags from `opts` to `args`.
/// `-L<dir>` and `-l<name>` map directly; `-rpath` is passed as a
/// pair; `-shared` switches output type; `-static` discourages
/// dynamic linking on supported platforms.
fn push_link_flags(args: &mut Vec<String>, opts: &Options) {
    for dir in &opts.library_search_paths {
        args.push(format!("-L{}", dir.display()));
    }
    for lib in &opts.link_libs {
        args.push(format!("-l{}", lib));
    }
    for path in &opts.rpath {
        args.push("-rpath".into());
        args.push(path.to_string_lossy().into_owned());
    }
    if opts.shared {
        args.push("-dylib".into());
    }
    if opts.static_link {
        // Apple ld doesn't have a true -static; the closest is
        // -search_paths_first to bias toward .a archives.  Keep the
        // intent visible without breaking link.
        args.push("-search_paths_first".into());
    }
}

fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Link multiple object files with the runtime to produce a binary.
fn link_multi(objs: &[PathBuf], output: &Path, opts: &Options) -> Result<(), String> {
    link_inputs(objs, output, opts)
}

/// Compile multiple Fortran source files with automatic dependency
/// resolution, producing a single linked binary.
///
/// 1. Scan all files for MODULE/USE dependencies.
/// 2. Topological sort (error on cycles).
/// 3. Compile each in order to a temp .o + .amod.
/// 4. Link all .o files into the output binary.
pub fn compile_multi(opts: &Options) -> Result<(), String> {
    let mut all_inputs = vec![opts.input.clone()];
    all_inputs.extend(opts.extra_inputs.iter().cloned());

    if opts.emit_obj && opts.output.is_some() {
        return Err("-o cannot be used with -c and multiple input files".into());
    }

    // Scan dependencies.
    let file_deps: Vec<dep_scan::FileDeps> = all_inputs
        .iter()
        .map(|p| dep_scan::scan_file(p))
        .collect::<Result<Vec<_>, _>>()?;

    // Topological sort.
    let order = dep_scan::resolve_compilation_order(&file_deps)?;

    // Compile each file in order.
    let tmp_dir = if opts.emit_obj {
        None
    } else {
        let dir = std::env::temp_dir().join(format!("afs_multi_{}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir: {}", e))?;
        Some(dir)
    };

    let mut object_files: Vec<PathBuf> = Vec::new();
    for &idx in &order {
        let src = &file_deps[idx].path;
        let obj_path = if opts.emit_obj {
            src.with_extension("o")
        } else {
            let tmp_dir = tmp_dir.as_ref().expect("temp dir for multi-file link");
            let stem = src
                .file_stem()
                .unwrap_or_default()
                .to_str()
                .unwrap_or("out");
            tmp_dir.join(format!("{}.o", stem))
        };

        // Build a single-file Options for this source by inheriting
        // the user-facing flags and overriding only the per-file bits.
        let mut sub_opts = Options {
            input: src.clone(),
            extra_inputs: vec![],
            output: Some(obj_path.clone()),
            emit_obj: true,
            ..Options::default()
        };
        sub_opts.opt_level = opts.opt_level;
        sub_opts.std = opts.std;
        sub_opts.source_form_override = opts.source_form_override;
        sub_opts.default_integer_8 = opts.default_integer_8;
        sub_opts.default_real_8 = opts.default_real_8;
        sub_opts.force_implicit_none = opts.force_implicit_none;
        sub_opts.recursive_default = opts.recursive_default;
        sub_opts.backslash_escapes = opts.backslash_escapes;
        sub_opts.max_stack_var_size = opts.max_stack_var_size;
        sub_opts.warn_all = opts.warn_all;
        sub_opts.warn_extra = opts.warn_extra;
        sub_opts.warn_pedantic = opts.warn_pedantic;
        sub_opts.warn_deprecated = opts.warn_deprecated;
        sub_opts.warn_as_error = opts.warn_as_error;
        sub_opts.disabled_warnings = opts.disabled_warnings.clone();
        sub_opts.debug_info = opts.debug_info;
        sub_opts.verbose = opts.verbose;
        sub_opts.time_report = opts.time_report;
        sub_opts.diagnostics_format = opts.diagnostics_format;
        sub_opts.check_bounds = opts.check_bounds;
        sub_opts.check_all = opts.check_all;
        sub_opts.module_output_dir = opts.module_output_dir.clone();
        sub_opts.module_search_paths = {
            let mut paths = opts.module_search_paths.clone();
            if let Some(tmp_dir) = tmp_dir.as_ref() {
                paths.push(tmp_dir.clone()); // find .amod from earlier compilations
            }
            paths
        };
        compile(&sub_opts)?;
        if !opts.emit_obj {
            object_files.push(obj_path);
        }
    }

    if opts.emit_obj {
        return Ok(());
    }

    // Link all object files.
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from("a.out"));
    link_multi(&object_files, &output, opts)?;

    // Cleanup.
    if let Some(tmp_dir) = tmp_dir {
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    Ok(())
}

/// Find libarmfortas_rt.a in common locations.
fn find_runtime_lib() -> Result<String, String> {
    // 1. $AFS_RUNTIME_PATH — the explicit override.  Accepts either
    //    a directory containing libarmfortas_rt.a or the archive
    //    path directly.
    if let Ok(env_path) = std::env::var("AFS_RUNTIME_PATH") {
        let p = PathBuf::from(&env_path);
        if p.is_dir() {
            let candidate = p.join("libarmfortas_rt.a");
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        } else if p.exists() {
            return Ok(env_path);
        }
    }

    // 2. Cargo workspace — when running out of the build tree.
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

    // 3. Sibling of the compiler binary:
    //      <bindir>/libarmfortas_rt.a
    //      <bindir>/../lib/libarmfortas_rt.a      (classic FHS)
    //      <bindir>/../lib/armfortas/libarmfortas_rt.a
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let candidates = [
            dir.join("libarmfortas_rt.a"),
            dir.join("../lib/libarmfortas_rt.a"),
            dir.join("../lib/armfortas/libarmfortas_rt.a"),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }

    // 4. Standard install locations.
    for fixed in &[
        "/usr/local/lib/libarmfortas_rt.a",
        "/usr/local/lib/armfortas/libarmfortas_rt.a",
        "/opt/homebrew/lib/libarmfortas_rt.a",
    ] {
        if Path::new(fixed).exists() {
            return Ok((*fixed).to_string());
        }
    }

    Err("cannot find libarmfortas_rt.a. Searched: \
         $AFS_RUNTIME_PATH, cargo workspace, next to the compiler \
         binary, and /usr/local/lib. Build with \
         'cargo build -p armfortas-rt' or set AFS_RUNTIME_PATH."
        .into())
}

fn find_libsystem_tbd() -> Result<String, String> {
    if let Some(path) = env_override("AFS_LIBSYSTEM_TBD") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(path);
        }
        return Err(format!(
            "AFS_LIBSYSTEM_TBD points to missing path '{}'",
            p.display()
        ));
    }

    let sdk = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .map_err(|e| format!("cannot run xcrun: {}", e))?;
    if !sdk.status.success() {
        return Err(format!(
            "xcrun failed:\n{}",
            String::from_utf8_lossy(&sdk.stderr)
        ));
    }
    let sysroot = String::from_utf8_lossy(&sdk.stdout).trim().to_string();
    let tbd = PathBuf::from(&sysroot).join("usr/lib/libSystem.tbd");
    if tbd.exists() {
        Ok(tbd.to_string_lossy().into_owned())
    } else {
        Err(format!("cannot find libSystem.tbd at '{}'", tbd.display()))
    }
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
    let archive_mtime = fs::metadata(&debug_archive)
        .ok()
        .and_then(|meta| meta.modified().ok());

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
            if ancestor.join("Cargo.toml").exists() && ancestor.join("runtime/Cargo.toml").exists()
            {
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

    #[test]
    fn default_standard_is_f2018() {
        assert_eq!(
            Options::default().std,
            Some(crate::sema::validate::FortranStandard::F2018)
        );
    }

    #[test]
    fn options_from_args_accepts_gnu_std_alias() {
        let args = vec!["-std=f2008".to_string(), "hello.f90".to_string()];
        let opts = Options::from_args(&args).expect("driver should accept -std=f2008");
        assert_eq!(
            opts.std,
            Some(crate::sema::validate::FortranStandard::F2008)
        );
    }

    #[test]
    fn parse_cli_warns_for_cpp_compat_flag() {
        let args = vec!["-cpp".to_string(), "hello.f90".to_string()];
        let ParsedCli::Compile(opts) = parse_cli(&args).expect("driver should accept -cpp") else {
            panic!("expected compile options");
        };
        assert!(opts.cpp_compat, "-cpp should be recorded on the options");
        assert!(
            opts.cli_warnings
                .iter()
                .any(|warning| warning.contains("-cpp is accepted for compatibility")),
            "expected a compatibility warning for -cpp, got {:?}",
            opts.cli_warnings
        );
    }

    #[test]
    fn parse_cli_warns_for_fbacktrace_flag() {
        let args = vec!["-fbacktrace".to_string(), "hello.f90".to_string()];
        let ParsedCli::Compile(opts) = parse_cli(&args).expect("driver should accept -fbacktrace")
        else {
            panic!("expected compile options");
        };
        assert!(
            opts.backtrace_requested,
            "-fbacktrace should be recorded on the options"
        );
        assert!(
            opts.cli_warnings.iter().any(|warning| warning.contains(
                "-fbacktrace is accepted, but runtime backtrace control is not yet implemented"
            )),
            "expected a compatibility warning for -fbacktrace, got {:?}",
            opts.cli_warnings
        );
    }

    #[test]
    fn parse_cli_warns_for_ffree_line_length_none_flag() {
        let args = vec![
            "-ffree-line-length-none".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) =
            parse_cli(&args).expect("driver should accept -ffree-line-length-none")
        else {
            panic!("expected compile options");
        };
        assert!(opts.free_line_length_none_compat);
        assert!(
            opts.cli_warnings
                .iter()
                .any(|warning| warning
                    .contains("-ffree-line-length-none is accepted for compatibility")),
            "expected a compatibility warning for -ffree-line-length-none, got {:?}",
            opts.cli_warnings
        );
    }

    #[test]
    fn parse_cli_accepts_module_alias() {
        let args = vec![
            "-module".to_string(),
            "mods".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) = parse_cli(&args).expect("driver should accept -module")
        else {
            panic!("expected compile options");
        };
        assert_eq!(opts.module_output_dir, Some(PathBuf::from("mods")));
        assert_eq!(opts.module_search_paths, vec![PathBuf::from("mods")]);
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("O0 --emit-ir should support integer(16) staging");
        let ir = fs::read_to_string(&output).expect("missing emitted IR");
        assert!(
            ir.contains("i128"),
            "emitted IR should expose integer(16) as i128:\n{}",
            ir
        );
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        let err =
            compile(&opts).expect_err("backend should reject integer(16) until i128 codegen lands");
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("simple integer(16) memory traffic should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(
            asm.contains("stp x16, x17"),
            "expected paired i128 store in asm:\n{}",
            asm
        );
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("simple integer(16) add should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(
            asm.contains("adds x16, x16, x8"),
            "expected i128 add carry chain in asm:\n{}",
            asm
        );
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("internal integer(16) call should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(
            asm.contains("bl _afs_internal___prog_integer16_internal_call_1"),
            "expected internal helper call in asm:\n{}",
            asm
        );
        assert!(
            asm.contains("stp x0, x1"),
            "expected pair-register i128 ABI spill in asm:\n{}",
            asm
        );
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("external integer(16) call should codegen at O0");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(
            asm.contains("bl _add_ext"),
            "expected external helper call in asm:\n{}",
            asm
        );
        assert!(
            asm.contains("stp x0, x1"),
            "expected pair-register i128 ABI spill in asm:\n{}",
            asm
        );
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
            extra_inputs: vec![],
            module_search_paths: vec![],
            ..Options::default()
        };

        compile(&opts).expect("integer(16) multiply should codegen at O1 after const fold");
        let asm = fs::read_to_string(&output).expect("missing emitted assembly");
        assert!(
            asm.contains("movz x16, #42"),
            "expected folded i128 constant in asm:\n{}",
            asm
        );
        assert!(
            !asm.contains("mul "),
            "expected O1 i128 multiply to fold away before backend:\n{}",
            asm
        );
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
