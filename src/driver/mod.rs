//! Compilation driver.
//!
//! CLI argument parsing, phase orchestration, multi-file compilation,
//! dependency resolution, and linker invocation.

pub mod conformance;
pub mod defaults;
pub mod dep_scan;
pub mod diag;
pub mod elf_crt;

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ir::inst::{InstKind, Module, RuntimeFunc};
use crate::ir::{lower, printer as ir_printer, verify};
use crate::lexer::{detect_source_form, tokenize_source_view, SourceForm, Span};
use crate::parser::Parser;
use crate::runtime::artifact::{
    find_source_workspace_from, fresh_runtime_lib, materialize_bundled_runtime,
    runtime_lib_candidate, RuntimeArchive, RuntimeProfile,
};
use crate::sema::{resolve, validate};

static NEXT_ATOMIC_WRITE_ID: AtomicU64 = AtomicU64::new(0);

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Transaction guard for an object or executable written directly by a
/// subprocess. Preparing removes any old destination; only a verified,
/// non-empty regular file survives the guard.
struct PendingExternalOutput {
    path: PathBuf,
    committed: bool,
}

impl PendingExternalOutput {
    fn prepare(path: &Path, tool: &str) -> Result<Self, String> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot remove stale {tool} output '{}': {error}",
                    path.display()
                ));
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            committed: false,
        })
    }

    fn verify(mut self, tool: &str) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            format!(
                "{tool} reported success but did not produce output '{}': {error}",
                self.path.display()
            )
        })?;
        if !metadata.file_type().is_file() || metadata.len() == 0 {
            return Err(format!(
                "{tool} reported success but did not produce a non-empty regular output '{}'",
                self.path.display()
            ));
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for PendingExternalOutput {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

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

    /// Whether multiply-add contraction may change floating-point rounding.
    pub fn fp_contract(self) -> bool {
        matches!(self, Self::Ofast)
    }
}

/// Source-form override requested on the command line.  None means
/// detect from the file extension (.f90 → free, .f / .for → fixed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormOverride {
    Free,
    Fixed,
}

/// Target CPU capability level (x10). One binary targets one ISA
/// level — no runtime dispatch. `Baseline` means the architectural
/// floor: SSE2 on x86_64, NEON on arm64. Higher levels
/// (x86-64-v2, avx2, ...) are reserved names until a sprint takes
/// them; parse rejects them so scripts fail loudly instead of
/// compiling at a level the user didn't get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetCpu {
    #[default]
    Baseline,
}

fn parse_target_cpu(name: &str) -> Result<TargetCpu, String> {
    match name {
        "baseline" => Ok(TargetCpu::Baseline),
        "x86-64-v2" | "x86-64-v3" | "avx2" | "avx512" => Err(format!(
            "--target-cpu={} is reserved but not implemented; only 'baseline' is supported",
            name
        )),
        other => Err(format!(
            "unknown --target-cpu '{}'; only 'baseline' is supported",
            other
        )),
    }
}

/// Action that should run when args parsing completes successfully
/// without producing a compile job (e.g. --help, --version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoAction {
    Help,
    Version,
    DumpVersion,
    /// Print the default (host) target triple. Scripts key per-target
    /// artifacts off this (x10: benchmark baselines).
    PrintTarget,
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
    UnsupportedSource,
}

/// One user-supplied operand in the original linker stream.
///
/// Source paths are replaced with their compiled object paths just before
/// linking, prebuilt artifacts pass through, and libraries retain their exact
/// position between those inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOperand {
    Input(PathBuf),
    Library(String),
}

/// User-supplied target text for a make dependency rule.
///
/// GNU-compatible `-MT` accepts make syntax verbatim, while `-MQ` quotes
/// make-special characters so the argument names a literal target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepTarget {
    Verbatim(String),
    Quoted(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalMode {
    Preprocess,
    Tokens,
    Ast,
    Ir,
    Assembly,
    Object,
}

impl TerminalMode {
    /// Return the first terminal phase reached by `compile_with_bundled_runtime`.
    ///
    /// Keeping this order aligned with that pipeline makes multi-input jobs
    /// obey the same mode precedence as single-input jobs even if callers
    /// construct `Options` directly with more than one mode bit set.
    fn from_options(opts: &Options) -> Option<Self> {
        if opts.preprocess_only {
            Some(Self::Preprocess)
        } else if opts.emit_tokens {
            Some(Self::Tokens)
        } else if opts.emit_ast {
            Some(Self::Ast)
        } else if opts.emit_ir {
            Some(Self::Ir)
        } else if opts.emit_asm {
            Some(Self::Assembly)
        } else if opts.emit_obj {
            Some(Self::Object)
        } else {
            None
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Preprocess => "-E",
            Self::Tokens => "--emit-tokens",
            Self::Ast => "--emit-ast",
            Self::Ir => "--emit-ir",
            Self::Assembly => "-S",
            Self::Object => "-c",
        }
    }

    fn requires_dependency_order(self) -> bool {
        matches!(self, Self::Ir | Self::Assembly | Self::Object)
    }

    fn configure_child(self, opts: &mut Options) {
        opts.preprocess_only = false;
        opts.emit_tokens = false;
        opts.emit_ast = false;
        opts.emit_ir = false;
        opts.emit_asm = false;
        opts.emit_obj = false;
        match self {
            Self::Preprocess => opts.preprocess_only = true,
            Self::Tokens => opts.emit_tokens = true,
            Self::Ast => opts.emit_ast = true,
            Self::Ir => opts.emit_ir = true,
            Self::Assembly => opts.emit_asm = true,
            Self::Object => opts.emit_obj = true,
        }
    }

    fn output_for_input(self, input: &Path) -> Option<PathBuf> {
        if self == Self::Preprocess {
            return None;
        }
        if self == Self::Object {
            return Some(input.with_extension("o"));
        }
        let stem = input
            .file_stem()
            .unwrap_or_default()
            .to_str()
            .unwrap_or("a");
        let extension = match self {
            Self::Tokens => "tokens",
            Self::Ast => "ast",
            Self::Ir => "ir",
            Self::Assembly => "s",
            Self::Preprocess | Self::Object => unreachable!(),
        };
        Some(PathBuf::from(format!("{stem}.{extension}")))
    }
}

fn terminal_output_collision_key(output: &Path, cwd: &Path) -> PathBuf {
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        cwd.join(output)
    };
    let normalized = normalize_path_lexically(&absolute);

    // Resolve an existing parent so equivalent spellings through a directory
    // symlink cannot evade the preflight. The output itself may not exist yet.
    match (normalized.parent(), normalized.file_name()) {
        (Some(parent), Some(file_name)) => fs::canonicalize(parent)
            .map(|parent| parent.join(file_name))
            .unwrap_or(normalized),
        _ => normalized,
    }
}

fn validate_unique_terminal_outputs(
    inputs: &[PathBuf],
    outputs: &[Option<PathBuf>],
) -> Result<(), String> {
    debug_assert_eq!(inputs.len(), outputs.len());
    if outputs.iter().all(Option::is_none) {
        return Ok(());
    }

    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot determine current directory: {error}"))?;
    let mut claimed = std::collections::HashMap::<PathBuf, usize>::with_capacity(outputs.len());
    for (index, output) in outputs.iter().enumerate() {
        let Some(output) = output else {
            continue;
        };
        let key = terminal_output_collision_key(output, &cwd);
        if let Some(first_index) = claimed.insert(key, index) {
            return Err(format!(
                "multiple input files '{}' and '{}' map to the same output '{}'; \
                 compile them separately or use distinct source basenames",
                inputs[first_index].display(),
                inputs[index].display(),
                output.display()
            ));
        }
    }
    Ok(())
}

/// Compilation options.
#[derive(Clone)]
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
    /// Emit a make-style dependency file (`-MD`/`-MMD`, optionally `-MF`).
    pub emit_depfile: bool,
    pub depfile: Option<PathBuf>,
    pub dep_targets: Vec<DepTarget>,
    pub depfile_phony: bool,

    // ---- Language ----
    pub std: Option<crate::sema::validate::FortranStandard>,
    /// True when the user passed `--std`/`-std` explicitly. Conformance
    /// warnings (source limits, l01) fire only in explicit-std runs —
    /// the gfortran model, where the default std is permissive and
    /// `-std=` opts into conformance mode. Acceptance is identical
    /// either way.
    pub std_explicit: bool,
    pub source_form_override: Option<SourceFormOverride>,
    pub default_integer_8: bool,
    pub default_real_8: bool,
    pub force_implicit_none: bool,
    pub recursive_default: bool,
    pub backslash_escapes: bool,
    pub free_line_length_limit: Option<usize>,
    pub free_line_length_none_compat: bool,
    pub max_stack_var_size: Option<u64>,
    pub max_errors_compat: Option<u64>,
    pub no_stack_arrays_compat: bool,
    pub check_array_temps_compat: bool,
    pub coarray_single_compat: bool,

    // ---- Optimization ----
    pub opt_level: OptLevel,

    // ---- Warnings ----
    pub warn_all: bool,
    pub warn_extra: bool,
    pub warn_pedantic: bool,
    pub warn_deprecated: bool,
    pub warn_as_error: bool,
    pub suppress_warnings: bool, // -w
    pub werror_implicit_interface_compat: bool,
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
    /// Positional inputs and `-l<name>` libraries in command-line order.
    pub link_operands: Vec<LinkOperand>,
    /// `-shared` / `-static`.
    pub shared: bool,
    pub static_link: bool,
    /// Linker flags accepted by compiler-driver compatibility spellings
    /// such as `-Wl,` and Darwin's `-install_name`.
    pub extra_link_args: Vec<String>,
    /// `-rpath` entries passed to `ld`.
    pub rpath: Vec<PathBuf>,

    // ---- Target ----
    /// What we compile for (`--target <triple>`; defaults to the host).
    pub target: crate::target::TargetSpec,
    /// Extra crt-object search directories (`-B <dir>` / `AFS_CRT_DIR`),
    /// searched before the built-in ELF probe list. The configuration
    /// path on layouts without an FHS crt location (NixOS).
    pub crt_search_dirs: Vec<PathBuf>,
    /// `-no-pie`: link a position-dependent executable (crt1.o, no
    /// -pie). ELF targets only; the default is PIE.
    pub no_pie: bool,
    /// ISA capability level (x10): baseline only today.
    pub target_cpu: TargetCpu,
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
            emit_depfile: false,
            depfile: None,
            dep_targets: Vec::new(),
            depfile_phony: false,
            std: Some(crate::sema::validate::FortranStandard::F2018),
            std_explicit: false,
            source_form_override: None,
            default_integer_8: false,
            default_real_8: false,
            force_implicit_none: false,
            recursive_default: false,
            backslash_escapes: false,
            free_line_length_limit: None,
            free_line_length_none_compat: false,
            max_stack_var_size: None,
            max_errors_compat: None,
            no_stack_arrays_compat: false,
            check_array_temps_compat: false,
            coarray_single_compat: false,
            opt_level: OptLevel::O0,
            warn_all: false,
            warn_extra: false,
            warn_pedantic: false,
            warn_deprecated: false,
            warn_as_error: false,
            suppress_warnings: false,
            werror_implicit_interface_compat: false,
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
            link_operands: Vec::new(),
            shared: false,
            static_link: false,
            extra_link_args: Vec::new(),
            rpath: Vec::new(),
            target: crate::target::TargetSpec::host(),
            crt_search_dirs: Vec::new(),
            no_pie: false,
            target_cpu: TargetCpu::Baseline,
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

    pub(crate) fn warnings_enabled(&self) -> bool {
        !self.suppress_warnings
    }

    pub(crate) fn warnings_are_errors(&self) -> bool {
        self.warnings_enabled() && self.warn_as_error
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
            "--print-target" => info_action = Some(InfoAction::PrintTarget),

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

            // ---- Target ----
            "--target" => {
                i += 1;
                let triple = args.get(i).ok_or("--target requires a triple")?;
                opts.target = crate::target::TargetSpec::parse(triple)?;
            }
            arg if arg.starts_with("--target=") => {
                opts.target = crate::target::TargetSpec::parse(&arg["--target=".len()..])?;
            }
            // ---- Target CPU capability (x10) ----
            // One binary targets one ISA level; runtime dispatch is
            // deliberately out of scope. Only the architectural
            // baseline is accepted today (SSE2 on x86_64, NEON on
            // arm64); names like x86-64-v2/avx2 are reserved.
            "--target-cpu" => {
                i += 1;
                let cpu = args.get(i).ok_or("--target-cpu requires a value")?;
                opts.target_cpu = parse_target_cpu(cpu)?;
            }
            arg if arg.starts_with("--target-cpu=") => {
                opts.target_cpu = parse_target_cpu(&arg["--target-cpu=".len()..])?;
            }
            "-B" => {
                i += 1;
                opts.crt_search_dirs
                    .push(PathBuf::from(args.get(i).ok_or("-B requires a directory")?));
            }
            arg if arg.starts_with("-B") => opts
                .crt_search_dirs
                .push(PathBuf::from(short_option_value(arg, "-B", "a directory")?)),
            "-no-pie" => opts.no_pie = true,

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
            "-isystem" => {
                i += 1;
                opts.module_search_paths.push(PathBuf::from(
                    args.get(i).ok_or("-isystem requires a directory")?,
                ));
            }
            arg if arg.starts_with("-isystem") => {
                opts.module_search_paths
                    .push(PathBuf::from(short_option_value(
                        arg,
                        "-isystem",
                        "a directory",
                    )?))
            }

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
                opts.link_operands.push(LinkOperand::Library(
                    args.get(i).ok_or("-l requires a library name")?.clone(),
                ));
            }
            arg if arg.starts_with("-l") => opts.link_operands.push(LinkOperand::Library(
                short_option_value(arg, "-l", "a library name")?.to_string(),
            )),

            "-rpath" | "--rpath" => {
                i += 1;
                opts.rpath
                    .push(PathBuf::from(args.get(i).ok_or("-rpath requires a path")?));
            }

            "-shared" | "-dynamiclib" => opts.shared = true,
            "-static" => opts.static_link = true,
            arg if arg.starts_with("-Wl,") => {
                opts.extra_link_args.extend(
                    arg["-Wl,".len()..]
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned),
                );
            }
            "-Xlinker" => {
                i += 1;
                opts.extra_link_args
                    .push(args.get(i).ok_or("-Xlinker requires an argument")?.clone());
            }
            "-install_name" | "-compatibility_version" | "-current_version" => {
                opts.extra_link_args.push(arg.clone());
                i += 1;
                opts.extra_link_args.push(
                    args.get(i)
                        .ok_or_else(|| format!("{} requires an argument", arg))?
                        .clone(),
                );
            }

            // ---- Standards / language flags ----
            arg if arg.starts_with("-std=") => {
                let val = &arg["-std=".len()..];
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown -std value: {}", val))?,
                );
                opts.std_explicit = true;
            }
            "-std" => {
                i += 1;
                let val = args.get(i).ok_or("-std requires a value")?;
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown -std value: {}", val))?,
                );
                opts.std_explicit = true;
            }
            arg if arg.starts_with("--std=") => {
                let val = &arg["--std=".len()..];
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown --std value: {}", val))?,
                );
                opts.std_explicit = true;
            }
            "--std" => {
                i += 1;
                let val = args.get(i).ok_or("--std requires a value")?;
                opts.std = Some(
                    crate::sema::validate::FortranStandard::parse_flag(val)
                        .ok_or_else(|| format!("unknown --std value: {}", val))?,
                );
                opts.std_explicit = true;
            }
            "-ffree-form" => opts.source_form_override = Some(SourceFormOverride::Free),
            "-ffixed-form" => opts.source_form_override = Some(SourceFormOverride::Fixed),
            "-ffree-line-length-none" => {
                opts.free_line_length_limit = None;
                opts.free_line_length_none_compat = true;
            }
            arg if arg.starts_with("-ffree-line-length-") => {
                opts.free_line_length_limit =
                    Some(parse_free_line_length(&arg["-ffree-line-length-".len()..])?);
                opts.free_line_length_none_compat = false;
            }
            arg if arg.starts_with("-ffree-line-length=") => {
                opts.free_line_length_limit =
                    Some(parse_free_line_length(&arg["-ffree-line-length=".len()..])?);
                opts.free_line_length_none_compat = false;
            }
            "-fdefault-integer-8" => opts.default_integer_8 = true,
            "-fdefault-real-8" => opts.default_real_8 = true,
            "-fimplicit-none" => opts.force_implicit_none = true,
            "-frecursive" => opts.recursive_default = true,
            "-fno-stack-arrays" => opts.no_stack_arrays_compat = true,
            "-fPIC" | "-fpic" | "-fPIE" | "-fpie" => {}
            "-fpreprocessed" | "-nocpp" => {}
            "-fbackslash" => opts.backslash_escapes = true,
            "-fno-backslash" => opts.backslash_escapes = false,
            arg if arg.starts_with("-fmax-stack-var-size=") => {
                let val = &arg["-fmax-stack-var-size=".len()..];
                opts.max_stack_var_size = Some(
                    val.parse()
                        .map_err(|_| format!("invalid -fmax-stack-var-size value: {}", val))?,
                );
            }
            arg if arg.starts_with("-fmax-errors=") => {
                let val = &arg["-fmax-errors=".len()..];
                opts.max_errors_compat = Some(
                    val.parse()
                        .map_err(|_| format!("invalid -fmax-errors value: {}", val))?,
                );
            }
            "-fcoarray=single" => opts.coarray_single_compat = true,

            // ---- Runtime checks ----
            "-fcheck=bounds" => opts.check_bounds = true,
            "-fcheck=array-temps" => opts.check_array_temps_compat = true,
            "-fcheck=all" => {
                opts.check_bounds = true;
                opts.check_all = true;
            }
            "-fbacktrace" => opts.backtrace_requested = true,

            // ---- Warnings (accepted; gating is gradual sprint work) ----
            "-Wall" => set_warning_option(&mut opts, "all", true),
            "-Wextra" => set_warning_option(&mut opts, "extra", true),
            "-Wpedantic" | "-pedantic" => set_warning_option(&mut opts, "pedantic", true),
            "-Wdeprecated" => set_warning_option(&mut opts, "deprecated", true),
            "-Werror" => set_warning_option(&mut opts, "error", true),
            "-Werror=implicit-interface" => opts.werror_implicit_interface_compat = true,
            arg if arg.starts_with("-Werror=") => {
                set_warning_option(&mut opts, "error", true);
                unknown_warning_flags.push(arg.to_string());
            }
            arg if arg.starts_with("-Wno-") => {
                set_warning_option(&mut opts, &arg[5..], false);
            }
            arg if arg.starts_with("-W") => {
                unknown_warning_flags.push(arg.to_string());
            }
            "-w" => opts.suppress_warnings = true,

            // ---- Make-style dependency-file compatibility ----
            "-MD" | "-MMD" => opts.emit_depfile = true,
            "-MP" => {
                opts.emit_depfile = true;
                opts.depfile_phony = true;
            }
            "-MF" => {
                i += 1;
                opts.emit_depfile = true;
                opts.depfile = Some(PathBuf::from(args.get(i).ok_or("-MF requires a file")?));
            }
            arg if arg.starts_with("-MF") => {
                opts.emit_depfile = true;
                opts.depfile = Some(PathBuf::from(short_option_value(arg, "-MF", "a file")?));
            }
            "-MT" | "-MQ" => {
                let flag = arg.clone();
                i += 1;
                opts.emit_depfile = true;
                let target = args
                    .get(i)
                    .ok_or_else(|| format!("{} requires a target", flag))?
                    .clone();
                opts.dep_targets.push(if flag == "-MQ" {
                    DepTarget::Quoted(target)
                } else {
                    DepTarget::Verbatim(target)
                });
            }
            arg if arg.starts_with("-MT") => {
                opts.emit_depfile = true;
                opts.dep_targets.push(DepTarget::Verbatim(
                    short_option_value(arg, "-MT", "a target")?.to_string(),
                ));
            }
            arg if arg.starts_with("-MQ") => {
                opts.emit_depfile = true;
                opts.dep_targets.push(DepTarget::Quoted(
                    short_option_value(arg, "-MQ", "a target")?.to_string(),
                ));
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
            arg if !arg.starts_with('-') => {
                let input = PathBuf::from(arg);
                opts.link_operands.push(LinkOperand::Input(input.clone()));
                inputs.push(input);
            }

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

    if is_darwin_loader_path_token(arg) {
        expanded.push(arg.to_string());
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

fn is_darwin_loader_path_token(arg: &str) -> bool {
    arg.starts_with("@rpath/")
        || arg.starts_with("@loader_path/")
        || arg.starts_with("@executable_path/")
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

fn parse_free_line_length(value: &str) -> Result<usize, String> {
    let limit: usize = value
        .parse()
        .map_err(|_| format!("invalid -ffree-line-length value: {}", value))?;
    if limit == 0 {
        return Err("-ffree-line-length requires a positive value".into());
    }
    Ok(limit)
}

fn set_warning_option(opts: &mut Options, name: &str, enabled: bool) {
    if enabled {
        opts.disabled_warnings.retain(|disabled| disabled != name);
    } else if !opts
        .disabled_warnings
        .iter()
        .any(|disabled| disabled == name)
    {
        opts.disabled_warnings.push(name.to_string());
    }

    let destination = match name {
        "all" => Some(&mut opts.warn_all),
        "extra" => Some(&mut opts.warn_extra),
        "pedantic" => Some(&mut opts.warn_pedantic),
        "deprecated" => Some(&mut opts.warn_deprecated),
        "error" => Some(&mut opts.warn_as_error),
        _ => None,
    };
    if let Some(destination) = destination {
        *destination = enabled;
    }
}

fn collect_cli_warnings(opts: &mut Options, unknown_warning_flags: &[String]) {
    if !opts.warnings_enabled() {
        return;
    }

    if opts.cpp_compat {
        opts.cli_warnings.push(
            "-cpp is accepted for compatibility; preprocessing already runs for Fortran inputs"
                .into(),
        );
    }

    if opts.check_all {
        opts.cli_warnings.push(
            "-fcheck=all is accepted, but only array bounds checks are implemented today".into(),
        );
    }

    if opts.max_stack_var_size.is_some() {
        opts.cli_warnings
            .push("-fmax-stack-var-size is recognized but not yet implemented".into());
    }
    if opts.max_errors_compat.is_some() {
        opts.cli_warnings.push(
            "-fmax-errors is recognized but diagnostic error limiting is not yet implemented"
                .into(),
        );
    }
    if opts.no_stack_arrays_compat {
        opts.cli_warnings.push(
            "-fno-stack-arrays is recognized but automatic array placement is not yet configurable"
                .into(),
        );
    }
    if opts.check_array_temps_compat {
        opts.cli_warnings.push(
            "-fcheck=array-temps is accepted for compatibility; array temporary diagnostics are not yet implemented".into(),
        );
    }
    if opts.coarray_single_compat {
        opts.cli_warnings.push(
            "-fcoarray=single is accepted for compatibility; coarray features are not yet implemented".into(),
        );
    }
    if opts.free_line_length_none_compat {
        opts.cli_warnings.push(
            "-ffree-line-length-none is accepted for compatibility; it silences the line-length conformance warning (free-form lines always compile in full regardless)".into(),
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
    if opts.werror_implicit_interface_compat {
        opts.cli_warnings.push(
            "-Werror=implicit-interface is accepted for compatibility; implicit-interface diagnostics are not yet implemented".into(),
        );
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

fn strip_bounds_check_calls(module: &mut Module) {
    for func in &mut module.functions {
        let mut changed = false;
        for block in &mut func.blocks {
            let before = block.insts.len();
            block.insts.retain(|inst| {
                !matches!(
                    inst.kind,
                    InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)
                )
            });
            changed |= block.insts.len() != before;
        }
        if changed {
            func.rebuild_type_cache();
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
  --target <triple>           Target to compile for (default: this machine).
                              Supported: arm64-macos, x86_64-freebsd,
                              x86_64-linux-gnu, x86_64-linux-musl
  --target-cpu <level>        ISA capability level; only 'baseline' today
                              (SSE2 on x86_64, NEON on arm64)
  --print-target              Print the default target triple and exit
  -B <dir>                    Extra crt-object search directory (ELF link;
                              also AFS_CRT_DIR). Required on layouts without
                              an FHS crt location, e.g. NixOS
  -no-pie                     Link a position-dependent executable (ELF;
                              default is PIE)

LANGUAGE:
  -std=<standard>             GNU-compatible alias for --std=<standard>
  --std=<standard>            Fortran standard (f77, f90, f95, f2003, f2008, f2018, f2023)
  -ffree-form                 Force free-form source
  -ffixed-form                Force fixed-form source
  -ffree-line-length-<n>      Override free-form line conformance warning limit
  -ffree-line-length-none     GNU-compatible alias; free-form inputs are already unlimited
  -fdefault-integer-8         Make default integer kind 8 bytes
  -fdefault-real-8            Make default real kind 8 bytes
  -fimplicit-none             Force implicit none in all scopes
  -frecursive                 Make all procedures recursive by default
  -fbackslash                 Interpret backslash in strings as escape
  -fmax-stack-var-size=<n>    Stack variable size threshold (bytes)
  -fmax-errors=<n>            GNU-compatible diagnostic limit spelling

OPTIMIZATION:
  -O0, -O1, -O2, -O3          Optimization level (default -O0)
  -Os                         Optimize for size
  -Ofast                      Aggressive optimization; permits floating-point
                              reassociation and multiply-add contraction

WARNINGS:
  -Wall                       All standard warnings
  -Wextra                     Extra warnings
  -Wpedantic                  Pedantic standard conformance warnings
  -Wdeprecated                Deprecated feature warnings
  -Werror                     Treat warnings as errors
  -w                          Suppress all warnings
  -Werror=implicit-interface  Accept GNU-style implicit-interface diagnostic flag
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
  -fcheck=array-temps         Accept GNU-style array-temp diagnostic flag
  -fcheck=all                 Enable all runtime checks
  -fcoarray=single            Accept GNU-style single-image coarray mode flag
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
        "{} {} ({})",
        program_name(),
        env!("CARGO_PKG_VERSION"),
        crate::target::TargetSpec::host()
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

fn all_input_paths(opts: &Options) -> Vec<PathBuf> {
    let mut inputs = vec![opts.input.clone()];
    inputs.extend(opts.extra_inputs.iter().cloned());
    inputs
}

fn resolve_link_operands(inputs: &[PathBuf], opts: &Options) -> Result<Vec<LinkOperand>, String> {
    // Programmatic callers historically supplied only `input` /
    // `extra_inputs`. Preserve that API by treating an empty stream as all
    // positional inputs and no libraries.
    if opts.link_operands.is_empty() {
        return Ok(inputs.iter().cloned().map(LinkOperand::Input).collect());
    }

    let input_slots = opts
        .link_operands
        .iter()
        .filter(|operand| matches!(operand, LinkOperand::Input(_)))
        .count();
    if input_slots != inputs.len() {
        return Err(format!(
            "internal error: linker operand stream has {} input slots for {} resolved inputs",
            input_slots,
            inputs.len()
        ));
    }

    let mut resolved_inputs = inputs.iter();
    opts.link_operands
        .iter()
        .map(|operand| match operand {
            LinkOperand::Input(_) => Ok(LinkOperand::Input(
                resolved_inputs
                    .next()
                    .expect("validated linker input slot count")
                    .clone(),
            )),
            LinkOperand::Library(name) => Ok(LinkOperand::Library(name.clone())),
        })
        .collect()
}

fn classify_cli_input(path: &Path) -> CliInputKind {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("o" | "obj" | "a" | "dylib" | "so") => CliInputKind::LinkArtifact,
        Some("c" | "cc" | "cpp" | "cxx" | "c++" | "h" | "hh" | "hpp" | "hxx" | "m" | "mm") => {
            CliInputKind::UnsupportedSource
        }
        _ => CliInputKind::FortranSource,
    }
}

fn unsupported_source_diagnostic(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext))
        .unwrap_or_else(|| "this extension".to_string());
    format!(
        "unsupported source file '{}': {} is not a Fortran source; armfortas only compiles Fortran sources. Compile C/C++/Objective-C inputs with the matching compiler, or pass --c-compiler/--cxx-compiler through the build system.",
        path.display(),
        ext
    )
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
    execute_with_bundled_runtime(opts, None)
}

pub(crate) fn execute_with_bundled_runtime(
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    let inputs = all_input_paths(opts);
    if let Some(input) = inputs
        .iter()
        .find(|path| classify_cli_input(path) == CliInputKind::UnsupportedSource)
    {
        return Err(unsupported_source_diagnostic(input));
    }
    let has_source = inputs
        .iter()
        .any(|path| classify_cli_input(path) == CliInputKind::FortranSource);
    let has_link_artifact = inputs
        .iter()
        .any(|path| classify_cli_input(path) == CliInputKind::LinkArtifact);

    match (has_source, has_link_artifact) {
        (true, false) => {
            if opts.extra_inputs.is_empty() {
                compile_with_bundled_runtime(opts, bundled_runtime)
            } else {
                compile_multi_with_bundled_runtime(opts, bundled_runtime)
            }
        }
        (false, true) => {
            validate_link_only_inputs(opts)?;
            let output = opts
                .output
                .clone()
                .unwrap_or_else(|| PathBuf::from("a.out"));
            link_inputs(&inputs, &output, opts, bundled_runtime)
        }
        // Mixed `foo.f90 bar.o libbaz.a -o prog`: gfortran/flang accept it:
        // compile the sources, then link the resulting objects together with
        // the prebuilt artifacts (in command order). compile_multi handles the
        // partition.
        (true, true) => compile_multi_with_bundled_runtime(opts, bundled_runtime),
        (false, false) => unreachable!("parse_cli guarantees at least one input"),
    }
}

fn source_form_for_input(opts: &Options, input: &Path) -> SourceForm {
    match opts.source_form_override {
        Some(SourceFormOverride::Free) => SourceForm::FreeForm,
        Some(SourceFormOverride::Fixed) => SourceForm::FixedForm,
        None => detect_source_form(&input.to_string_lossy()),
    }
}

fn preproc_config_for_input(
    opts: &Options,
    input: &Path,
    source_form: SourceForm,
) -> crate::preprocess::PreprocConfig {
    let mut config = crate::preprocess::PreprocConfig {
        filename: input.to_str().unwrap_or("<input>").to_string(),
        fixed_form: matches!(source_form, SourceForm::FixedForm),
        cpp_compat: opts.cpp_compat,
        // Share `-I` paths with the preprocessor so `#include "foo.inc"`
        // can find headers after searching relative to the current file.
        include_paths: opts.module_search_paths.clone(),
        ..crate::preprocess::PreprocConfig::for_target(&opts.target)
    };
    for (name, value) in &opts.preprocessor_defines {
        config
            .defines
            .insert(name.clone(), crate::preprocess::MacroDef::object(value));
    }
    config
}

/// Compile a Fortran source file through the full pipeline.
fn render_preprocessed_diagnostic(
    preprocessed: &crate::preprocess::PreprocOutput,
    span: Span,
    level: diag::Level,
    message: &str,
) {
    let resolved = preprocessed.resolve_span(span);
    let span_len = if resolved.source_span.end.line == resolved.source_span.start.line
        && resolved.source_span.end.col > resolved.source_span.start.col
    {
        (resolved.source_span.end.col - resolved.source_span.start.col) as usize
    } else {
        1
    };
    diag::render_mapped(
        resolved.filename,
        resolved.source,
        resolved.display_span,
        resolved.source_span,
        level,
        message,
        span_len,
    );
}

fn write_stdout_bytes(bytes: &[u8]) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    std::io::Write::write_all(&mut stdout, bytes)
        .map_err(|e| format!("cannot write standard output: {}", e))
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => normalized.push(component.as_os_str()),
            },
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn path_components_equal(left: Component<'_>, right: Component<'_>) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn path_anchor_len(components: &[Component<'_>]) -> Option<usize> {
    match components {
        [Component::Prefix(_), Component::RootDir, ..] => Some(2),
        [Component::RootDir, ..] => Some(1),
        _ => None,
    }
}

fn relative_path_with_shared_parent(input: &Path, cwd: &Path) -> Option<PathBuf> {
    let input_components = input.components().collect::<Vec<_>>();
    let cwd_components = cwd.components().collect::<Vec<_>>();
    let input_anchor_len = path_anchor_len(&input_components)?;
    let cwd_anchor_len = path_anchor_len(&cwd_components)?;
    if input_anchor_len != cwd_anchor_len
        || input_components[..input_anchor_len]
            .iter()
            .copied()
            .zip(cwd_components[..cwd_anchor_len].iter().copied())
            .any(|(left, right)| !path_components_equal(left, right))
    {
        return None;
    }

    let mut common = input_anchor_len;
    while common < input_components.len()
        && common < cwd_components.len()
        && path_components_equal(input_components[common], cwd_components[common])
    {
        common += 1;
    }

    // Do not encode unrelated absolute hierarchies merely because they share
    // a filesystem root. A meaningful common parent keeps build/source sibling
    // layouts reproducible while unrelated roots fall back to the basename.
    if common == input_anchor_len && common < cwd_components.len() {
        return None;
    }

    let mut relative = PathBuf::new();
    for _ in &cwd_components[common..] {
        relative.push("..");
    }
    for component in &input_components[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn module_source_provenance_from_absolute(input: &Path, cwd: &Path) -> String {
    let cwd = normalize_path_lexically(cwd);
    let input = normalize_path_lexically(input);

    relative_path_with_shared_parent(&input, &cwd)
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| {
            relative
                .iter()
                .map(|component| component.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .or_else(|| {
            input
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "source".to_string())
}

fn module_source_provenance(input: &Path) -> String {
    let fallback = || {
        input
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "source".to_string())
    };
    let Ok(input) = std::path::absolute(input) else {
        return fallback();
    };
    let Ok(cwd) = std::env::current_dir() else {
        return fallback();
    };
    module_source_provenance_from_absolute(&input, &cwd)
}

pub fn compile(opts: &Options) -> Result<(), String> {
    compile_with_bundled_runtime(opts, None)
}

fn compile_with_bundled_runtime(
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    compile_with_bundled_runtime_inner(opts, bundled_runtime, None)
}

fn compile_with_bundled_runtime_and_dependencies(
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<Vec<PathBuf>, String> {
    let mut dependencies = Vec::new();
    compile_with_bundled_runtime_inner(opts, bundled_runtime, Some(&mut dependencies))?;
    Ok(dependencies)
}

fn compile_with_bundled_runtime_inner(
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
    dependency_output: Option<&mut Vec<PathBuf>>,
) -> Result<(), String> {
    if TerminalMode::from_options(opts).is_none() {
        validate_dependency_file_destination(opts, &opts.output_path(), &[])?;
    }

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
    // Keep exact bytes for preprocessing and a marker-free display view for
    // source-limit diagnostics emitted before preprocessing.
    let raw = fs::read(&opts.input)
        .map_err(|e| format!("cannot read '{}': {}", opts.input.display(), e))?;
    let source_provenance = module_source_provenance(&opts.input);
    let source =
        crate::source_bytes::display_source_view(&crate::source_bytes::to_source_view(&raw));
    phase.end(&mut phases);
    let file_str = opts.input.display().to_string();

    // 2. Preprocess.
    let source_form = source_form_for_input(opts, &opts.input);
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
    // Source-limit conformance warnings (l01): one scan over the raw
    // source, before either continuation joiner runs. Explicit-std
    // runs only — the default std is permissive (gfortran's -std=gnu
    // model); a default build's stderr stays pristine.
    if let (true, true, Some(std)) = (opts.warnings_enabled(), opts.std_explicit, opts.std) {
        for w in conformance::check_source_limits(
            &source,
            std,
            source_form,
            opts.free_line_length_none_compat,
            opts.free_line_length_limit,
        ) {
            diag::render(&file_str, &source, w.span, diag::Level::Warning, &w.msg, 1);
        }
    }
    // Unconditional cap (all --std levels): keeps every recursive
    // walker's depth under the compile-thread stack reservation, so an
    // oversized statement gets this diagnostic, never a stack fault.
    if let Some((span, chars)) = conformance::find_over_cap_statement(&source, source_form) {
        diag::render(
            &file_str,
            &source,
            span,
            diag::Level::Error,
            &format!(
                "statement is {} characters long, over the {}-character compiler limit \
                 (the F2023 standard caps statements at 1,000,000 characters)",
                chars,
                conformance::STMT_HARD_CAP
            ),
            1,
        );
        return Err(format!(
            "aborting due to errors in {}",
            opts.input.display()
        ));
    }

    let phase = phases.start("preprocess");
    let pp_config = preproc_config_for_input(opts, &opts.input, source_form);
    let pp_result =
        crate::preprocess::preprocess_bytes(&raw, &pp_config).map_err(|e| format!("{}", e))?;
    phase.end(&mut phases);
    if let Some(dependencies) = dependency_output {
        dependencies.clone_from(&pp_result.included_files);
    }
    let included_files = &pp_result.included_files;
    if TerminalMode::from_options(opts).is_none() {
        prepare_dependency_file(opts, &opts.output_path(), included_files)?;
    }
    let preprocessed = pp_result.text.as_str();

    if opts.preprocess_only {
        let preprocessed_bytes = pp_result.bytes();
        if opts.output.is_none() {
            write_stdout_bytes(&preprocessed_bytes)?;
        } else {
            let out = opts.output_path();
            if out.as_os_str() == "-" {
                write_stdout_bytes(&preprocessed_bytes)?;
            } else {
                fs::write(&out, &preprocessed_bytes)
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
    let tokens = match tokenize_source_view(preprocessed, 0, source_form) {
        Ok(tokens) => tokens,
        Err(e) => {
            phase.end(&mut phases);
            render_preprocessed_diagnostic(
                &pp_result,
                e.span,
                diag::Level::Error,
                &format!("lexer error: {}", e.msg),
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
    let mut parser = Parser::new_source_view(&tokens);
    let mut units = match parser.parse_file() {
        Ok(units) => units,
        Err(e) => {
            phase.end(&mut phases);
            render_preprocessed_diagnostic(
                &pp_result,
                e.span,
                diag::Level::Error,
                &format!("parse error: {}", e.msg),
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
    let target_layout = crate::target::TargetLayout::of(&opts.target);
    let resolve_result =
        match resolve::resolve_file(&units, &opts.module_search_paths, target_layout) {
            Ok(result) => result,
            Err(e) => {
                phase.end(&mut phases);
                render_preprocessed_diagnostic(&pp_result, e.span, diag::Level::Error, &e.msg);
                phases.report();
                return Err(format!(
                    "aborting due to errors in {}",
                    opts.input.display()
                ));
            }
        };
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
        opts.warnings_enabled() && opts.warn_pedantic,
        opts.warnings_enabled() && opts.warn_deprecated,
    );
    phase.end(&mut phases);
    let mut had_error = false;
    for d in &diags {
        if d.kind == validate::DiagKind::Warning && !opts.warnings_enabled() {
            continue;
        }
        let level = match d.kind {
            validate::DiagKind::Error => diag::Level::Error,
            validate::DiagKind::Warning => diag::Level::Warning,
        };
        render_preprocessed_diagnostic(&pp_result, d.span, level, &d.msg);
        match d.kind {
            validate::DiagKind::Error => had_error = true,
            validate::DiagKind::Warning if opts.warnings_are_errors() => had_error = true,
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

    // Resolve TYPEOF/CLASSOF declaration specs to the concrete types
    // sema recorded, so every lowering pre-pass sees the same spec the
    // resolver did (F2023 l03).
    lower::normalize_typeof_specs(&mut units, &st);

    let (mut ir_module, module_globals) = lower::lower_file(
        &units,
        &st,
        &type_layouts,
        external_globals,
        external_optional_params,
        external_descriptor_params,
        external_char_len_star,
        target_layout,
    );
    if !opts.check_bounds {
        strip_bounds_check_calls(&mut ir_module);
    }
    let ir_errors = verify::verify_module(&ir_module);
    if !ir_errors.is_empty() {
        if std::env::var_os("AFS_DUMP_BAD_IR").is_some() {
            let path = std::env::temp_dir().join("afs_failed.ir");
            let _ = std::fs::write(&path, crate::ir::printer::print_module(&ir_module));
            eprintln!("afs: dumped failing IR to {}", path.display());
        }
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
            crate::opt::build_i128_pipeline(ir_opt, opts.target.arch).ok_or_else(|| {
                format!(
                    "integer(16) / i128 optimization at -{} is not yet supported; use --emit-ir to inspect the raw IR for now",
                    opts.opt_level.as_flag()
                )
            })?
        } else {
            crate::opt::build_pipeline(ir_opt, opts.target.arch)
        };
        pm.run(&mut ir_module)
            .map_err(|error| format!("internal error: {error}"))?;
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

    // 7-9. Backend: instruction selection, register allocation, and
    // assembly emission, dispatched on the target arch (x03). For
    // x86_64 this errors naming sprint x05.
    let phase = phases.start("codegen");
    let asm_text = crate::codegen::emit_module(&ir_module, opts)?;
    phase.end(&mut phases);
    if opts.emit_asm {
        let out = opts.output_path();
        fs::write(&out, &asm_text)
            .map_err(|e| format!("cannot write '{}': {}", out.display(), e))?;
        write_dependency_file(opts, &out, included_files)?;
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
    //       same .o name; otherwise the embedded string varies and
    //       reproducible-build tests fail.  PID is unsafe here
    //       because each compile_binary call spawns a fresh
    //       subprocess with a different PID.
    //   (2) Two parallel compiles of two DIFFERENT sources with the
    //       same basename (e.g. both writing `mod.o` to different
    //       unique-dir test outputs) must NOT race on the same temp
    //       file.  Output stem alone is therefore not enough.
    // Hash the absolute output identity with FNV-1a and use it in the
    // temp basename. Same output path means same hash and deterministic
    // OSO strings; same relative `-o t` from different directories means
    // different hashes and no cross-process temp-object collision.
    // std::collections::hash_map::DefaultHasher uses a per-process random
    // seed and would defeat (1).
    let out_path = opts.output_path();
    let temp_identity_path = if out_path.is_absolute() {
        out_path.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot determine current directory for temp output: {}", e))?
            .join(&out_path)
    };
    let token = {
        let bytes = temp_identity_path.as_os_str().as_encoded_bytes();
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    };
    let asm_path = std::env::temp_dir().join(format!("armfortas_{:016x}.s", token));
    let obj_path = if opts.emit_obj {
        out_path.clone()
    } else {
        std::env::temp_dir().join(format!("armfortas_{:016x}.o", token))
    };

    let _asm_cleanup = RemoveFileOnDrop(asm_path.clone());
    let _obj_cleanup = (!opts.emit_obj).then(|| RemoveFileOnDrop(obj_path.clone()));
    fs::write(&asm_path, &asm_text).map_err(|e| format!("cannot write temp assembly: {}", e))?;

    // x05: ELF targets assemble with the system assembler when the
    let local_descriptor_params = crate::ir::lower::collect_descriptor_params_for_units(&units);
    let local_char_len_star_params =
        crate::ir::lower::collect_char_len_star_params_for_units(&units);

    let module_artifact_dir: std::path::PathBuf =
        opts.module_output_dir.clone().unwrap_or_else(|| {
            if opts.emit_obj {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            } else {
                opts.output_path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf()
            }
        });

    // Emit module interface files for each MODULE in the compilation unit.
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
                    &source_provenance,
                    &raw,
                    &st,
                    mod_scope_id,
                    &module_globals,
                    &type_layouts,
                    &ir_module,
                    &local_descriptor_params,
                    &local_char_len_star_params,
                );
                let amod_path = module_artifact_dir.join(format!("{}.amod", mod_key));
                write_module_file_atomic(&amod_path, &amod_text)?;
                // `.amod` remains the ARMFORTAS module ABI file. A
                // byte-identical `.mod` alias keeps conventional Fortran
                // build systems such as CMake able to track module
                // dependencies for unknown compilers.
                let mod_path = module_artifact_dir.join(format!("{}.mod", mod_key));
                write_module_file_atomic(&mod_path, &amod_text)?;
                if opts.verbose {
                    eprintln!(" amod: {}", amod_path.display());
                }
            }
        }
    }
    for unit in &units {
        if let crate::ast::unit::ProgramUnit::Submodule {
            parent,
            ancestor,
            name,
            ..
        } = &unit.node
        {
            let parent_key = parent.to_lowercase();
            let name_key = name.to_lowercase();
            let parent_spec = if let Some(ancestor) = ancestor {
                format!("{}:{}", parent_key, ancestor.to_lowercase())
            } else {
                parent_key.clone()
            };
            let artifact_stem = format!("{}@{}", parent_key, name_key);
            let interface_name = format!("{}.amod", artifact_stem);
            let submodule_scope_id =
                st.find_submodule_scope(&parent_key, &name_key)
                    .ok_or_else(|| {
                        format!(
                            "cannot emit interface for unresolved submodule '{}:{}'",
                            parent_key, name_key
                        )
                    })?;
            let interface_text = crate::sema::amod::write_amod(
                name,
                &source_provenance,
                &raw,
                &st,
                submodule_scope_id,
                &module_globals,
                &type_layouts,
                &ir_module,
                &local_descriptor_params,
                &local_char_len_star_params,
            );
            let interface_fingerprint = crate::sema::amod::artifact_fingerprint(&interface_text);
            let interface_path = module_artifact_dir.join(&interface_name);
            write_module_file_atomic(&interface_path, &interface_text)?;
            if opts.verbose {
                eprintln!(" amod: {}", interface_path.display());
            }
            let smod_text = format!(
                "#!smod {}\n# compiler: armfortas {}\n# source: {}\n@parent {}\n@submodule {}\n@interface {} fnv1a:{}\n",
                crate::sema::amod::SMOD_VERSION,
                env!("CARGO_PKG_VERSION"),
                source_provenance,
                parent_spec,
                name_key,
                interface_name,
                interface_fingerprint
            );
            let smod_path = module_artifact_dir.join(format!("{}.smod", artifact_stem));
            write_module_file_atomic(&smod_path, &smod_text)?;
            if opts.verbose {
                eprintln!(" smod: {}", smod_path.display());
            }
        }
    }

    // ELF assembly routing (x14): the in-process afs-as x86 pipeline
    // is the default. AFS_AS_PATH substitutes a subprocess assembler
    // (invoked `<as> --64 -o obj asm`); AFS_AS=0 forces the system
    // `as`. Mach-O uses the embedded ARM64 assembler when crossing
    // host architecture or object format, and keeps native overrides below.
    if opts.target.object_format() == crate::target::ObjectFormat::Elf {
        let route = elf_assembler_override();
        let host = crate::target::TargetSpec::host();
        let cross = host.arch != opts.target.arch
            || host.object_format() != crate::target::ObjectFormat::Elf;
        if cross {
            if route.is_some() {
                return Err(format!(
                    "cannot assemble for target '{}' with a host assembler: unset AFS_AS_PATH/AFS_AS to use the built-in one",
                    opts.target
                ));
            }
            if !opts.emit_obj {
                return Err(format!(
                    "cannot link for target '{}' on this host: cross-linking is not supported, use -c",
                    opts.target
                ));
            }
        }
        let phase = phases.start("assemble");
        match &route {
            None => {
                // In-process: parse + encode + relax, then write the
                // ELF object directly.
                let src = fs::read_to_string(&asm_path)
                    .map_err(|e| format!("cannot read '{}': {}", asm_path.display(), e))?;
                let osabi = match opts.target.os {
                    crate::target::Os::FreeBsd => afs_as::elf::ELFOSABI_FREEBSD,
                    _ => afs_as::elf::ELFOSABI_NONE,
                };
                let obj = afs_as::x86::assemble::assemble_x86(&src, osabi)
                    .map_err(|e| format!("afs-as: {}: {}", asm_path.display(), e))?;
                let bytes = afs_as::elf::write_elf(&obj)
                    .map_err(|e| format!("afs-as: elf writer: {}", e))?;
                fs::write(&obj_path, bytes)
                    .map_err(|e| format!("cannot write '{}': {}", obj_path.display(), e))?;
                phase.end(&mut phases);
            }
            Some(assembler) => {
                let pending_output = PendingExternalOutput::prepare(&obj_path, "assembler")?;
                let as_result = Command::new(assembler)
                    .args(["--64", "-o"])
                    .arg(&obj_path)
                    .arg(&asm_path)
                    .output()
                    .map_err(|e| format!("cannot run assembler: {}", e))?;
                phase.end(&mut phases);
                if !as_result.status.success() {
                    return Err(format!(
                        "assembler failed:\n{}",
                        String::from_utf8_lossy(&as_result.stderr)
                    ));
                }
                pending_output.verify("assembler")?;
            }
        }
        if opts.emit_obj {
            write_dependency_file(opts, &obj_path, included_files)?;
            if opts.verbose {
                eprintln!(" assembled: {}", obj_path.display());
            }
            return Ok(());
        }
        let binary_path = opts.output_path();
        let phase = phases.start("link");
        let result = link_inputs(
            std::slice::from_ref(&obj_path),
            &binary_path,
            opts,
            bundled_runtime,
        );
        phase.end(&mut phases);
        result?;
        if opts.verbose {
            eprintln!(" linked: {}", binary_path.display());
        }
        write_link_dependency_file(
            opts,
            &binary_path,
            std::slice::from_ref(&opts.input),
            included_files,
        )?;
        phases.report();
        return Ok(());
    }

    let phase = phases.start("assemble");
    let host = crate::target::TargetSpec::host();
    let cross =
        host.arch != opts.target.arch || host.object_format() != crate::target::ObjectFormat::MachO;
    let assembler = env_override("AFS_AS_PATH");
    let assemble_result = if cross && assembler.is_none() {
        afs_as::assemble::assemble_file(&asm_path, &obj_path).map_err(|e| format!("afs-as: {}", e))
    } else {
        let assembler = assembler.unwrap_or_else(|| "as".into());
        let pending_output = PendingExternalOutput::prepare(&obj_path, "assembler")?;
        let result = Command::new(assembler)
            .arg(&asm_path)
            .arg("-o")
            .arg(&obj_path)
            .output()
            .map_err(|e| format!("cannot run assembler: {}", e))?;
        if result.status.success() {
            pending_output.verify("assembler")
        } else {
            Err(format!(
                "assembler failed:\n{}",
                String::from_utf8_lossy(&result.stderr)
            ))
        }
    };
    phase.end(&mut phases);
    assemble_result?;
    if opts.verbose {
        eprintln!(" assembled: {}", obj_path.display());
    }

    if opts.emit_obj {
        write_dependency_file(opts, &obj_path, included_files)?;
        phases.report();
        return Ok(());
    }

    // 11. Link.
    let binary_path = opts.output_path();
    let phase = phases.start("link");
    link(&obj_path, &binary_path, opts, bundled_runtime)?;
    phase.end(&mut phases);
    if opts.verbose {
        eprintln!(" linked: {}", binary_path.display());
    }
    write_link_dependency_file(
        opts,
        &binary_path,
        std::slice::from_ref(&opts.input),
        included_files,
    )?;

    phases.report();
    Ok(())
}

fn write_module_file_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("module");
    let id = NEXT_ATOMIC_WRITE_ID.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{}.{}.{}.tmp", file_name, std::process::id(), id));

    fs::write(&tmp, contents)
        .map_err(|e| format!("cannot write temporary '{}': {}", tmp.display(), e))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "cannot replace '{}' atomically: {}",
            path.display(),
            e
        ));
    }
    Ok(())
}

fn dependency_file_path(opts: &Options, output: &Path) -> Option<PathBuf> {
    if !opts.emit_depfile && opts.depfile.is_none() {
        return None;
    }
    Some(opts.depfile.clone().unwrap_or_else(|| {
        let mut path = output.to_path_buf();
        path.set_extension("d");
        path
    }))
}

fn validate_dependency_file_destination(
    opts: &Options,
    output: &Path,
    included_files: &[PathBuf],
) -> Result<(), String> {
    let Some(depfile) = dependency_file_path(opts, output) else {
        return Ok(());
    };
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot determine current directory: {error}"))?;
    let depfile_key = terminal_output_collision_key(&depfile, &cwd);
    if depfile_key == terminal_output_collision_key(output, &cwd) {
        return Err(format!(
            "dependency file '{}' conflicts with output '{}'",
            depfile.display(),
            output.display()
        ));
    }
    for input in all_input_paths(opts).iter().chain(included_files) {
        if depfile_key == terminal_output_collision_key(input, &cwd) {
            return Err(format!(
                "dependency file '{}' conflicts with compiler input '{}'",
                depfile.display(),
                input.display()
            ));
        }
    }
    Ok(())
}

fn prepare_dependency_file(
    opts: &Options,
    output: &Path,
    included_files: &[PathBuf],
) -> Result<(), String> {
    let Some(depfile) = dependency_file_path(opts, output) else {
        return Ok(());
    };
    validate_dependency_file_destination(opts, output, included_files)?;
    match fs::remove_file(&depfile) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot remove stale dependency file '{}': {error}",
            depfile.display()
        )),
    }
}

fn write_dependency_file(
    opts: &Options,
    output: &Path,
    included_files: &[PathBuf],
) -> Result<(), String> {
    write_dependency_file_for_sources(
        opts,
        output,
        std::slice::from_ref(&opts.input),
        included_files,
    )
}

fn write_dependency_file_for_sources(
    opts: &Options,
    output: &Path,
    source_inputs: &[PathBuf],
    included_files: &[PathBuf],
) -> Result<(), String> {
    let Some(depfile) = dependency_file_path(opts, output) else {
        return Ok(());
    };
    validate_dependency_file_destination(opts, output, included_files)?;
    if let Some(parent) = depfile.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "cannot create depfile directory '{}': {}",
                    parent.display(),
                    e
                )
            })?;
        }
    }

    let mut body = String::new();
    if opts.dep_targets.is_empty() {
        body.push_str(&escape_make_dep_token(&output.to_string_lossy()));
    } else {
        for (idx, target) in opts.dep_targets.iter().enumerate() {
            if idx > 0 {
                body.push(' ');
            }
            match target {
                DepTarget::Verbatim(target) => body.push_str(target),
                DepTarget::Quoted(target) => body.push_str(&escape_make_dep_token(target)),
            }
        }
    }
    body.push_str(": ");
    for (index, input) in source_inputs.iter().enumerate() {
        if index > 0 {
            body.push(' ');
        }
        body.push_str(&escape_make_dep_token(&input.to_string_lossy()));
    }
    for include in included_files {
        body.push(' ');
        body.push_str(&escape_make_dep_token(&include.to_string_lossy()));
    }
    body.push('\n');
    if opts.depfile_phony {
        for include in included_files {
            body.push('\n');
            body.push_str(&escape_make_dep_token(&include.to_string_lossy()));
            body.push_str(":\n");
        }
    }

    write_dependency_file_atomic(&depfile, &body)
}

fn write_dependency_file_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dependencies");
    let id = NEXT_ATOMIC_WRITE_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), id));
    let _cleanup = RemoveFileOnDrop(temporary.clone());

    fs::write(&temporary, contents).map_err(|error| {
        format!(
            "cannot write temporary dependency file '{}': {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "cannot replace dependency file '{}' atomically: {error}",
            path.display()
        )
    })
}

fn write_link_dependency_file(
    opts: &Options,
    output: &Path,
    source_inputs: &[PathBuf],
    included_files: &[PathBuf],
) -> Result<(), String> {
    if let Err(error) =
        write_dependency_file_for_sources(opts, output, source_inputs, included_files)
    {
        return match fs::remove_file(output) {
            Ok(()) => Err(error),
            Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {
                Err(error)
            }
            Err(cleanup_error) => Err(format!(
                "{error}; additionally cannot remove failed link output '{}': {cleanup_error}",
                output.display()
            )),
        };
    }
    Ok(())
}

fn escape_make_dep_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for ch in token.chars() {
        match ch {
            ' ' | '\t' | '#' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '$' => out.push_str("$$"),
            _ => out.push(ch),
        }
    }
    out
}

/// Link an object file with the runtime library to produce a binary.
/// `opts` contributes the user-supplied `-L`, `-l`, `-rpath`,
/// `-shared`, and `-static` flags that need to make it through to the linker.
fn link(
    obj: &Path,
    output: &Path,
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    link_inputs(&[obj.to_path_buf()], output, opts, bundled_runtime)
}

/// Link prebuilt objects and archives with the runtime to produce a
/// binary or shared library, preserving the user-supplied input order.
/// Link ELF objects into a dynamically linked PIE by invoking the
/// system `ld` directly (sprint x06). The driver owns the whole link
/// line — crt discovery, dynamic linker, library order — and no `cc`
/// appears anywhere in the pipeline. Flag surface sticks to the
/// lld/bfd intersection (FreeBSD ld is lld, Linux usually GNU bfd).
pub(crate) fn link_inputs_elf(
    inputs: &[PathBuf],
    output: &Path,
    opts: &Options,
) -> Result<(), String> {
    link_inputs_elf_with_bundled_runtime(inputs, output, opts, None)
}

fn link_inputs_elf_with_bundled_runtime(
    inputs: &[PathBuf],
    output: &Path,
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    let host = crate::target::TargetSpec::host();
    if host.arch != opts.target.arch
        || host.object_format() != crate::target::ObjectFormat::Elf
        || host.os != opts.target.os
    {
        return Err(format!(
            "cannot link for target '{}' on host '{}': cross-linking is out of scope for this arc (native builds only)",
            opts.target, host
        ));
    }
    if opts.static_link {
        return Err("-static on ELF targets lands in sprint x11 (musl static story)".to_string());
    }
    if opts.shared {
        return Err(
            "-shared on ELF targets is a follow-up after x06 (executables only this sprint)"
                .to_string(),
        );
    }

    let linker_override = afs_ld_override();
    // afs-ld's ELF backend currently emits ET_EXEC, not PIE. Route its
    // links through the matching crt1/crtbegin pair instead of combining
    // Scrt1/crtbeginS with a non-PIE output.
    let pie = !opts.no_pie && linker_override.is_none();
    let mut override_dirs = opts.crt_search_dirs.clone();
    // Colon-separated, PATH-style: NixOS needs two crt roots (crt1/
    // crti/crtn from glibc, crtbegin/crtend from the gcc store path).
    if let Some(dirs) = env_override("AFS_CRT_DIR") {
        override_dirs.extend(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    }
    let crt = elf_crt::find_crt(&opts.target, &override_dirs, pie)?;
    let runtime = find_runtime_lib(bundled_runtime)?;
    // LIBRARY_PATH: the cc-compatible -L env knob. On NixOS libgcc_s
    // lives in a third store path (gcc's -libgcc output) that no crt
    // root covers.
    let mut lib_paths = opts.library_search_paths.clone();
    if let Some(dirs) = env_override("LIBRARY_PATH") {
        lib_paths.extend(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    }
    let operands = resolve_link_operands(inputs, opts)?;
    let args = elf_crt::elf_link_args(
        &opts.target,
        &crt,
        &operands,
        runtime.path(),
        output,
        pie,
        &lib_paths,
    )?;

    // x16: honor the AFS_LD routing on ELF targets too — previously
    // this path went straight to the system linker and
    // afs_ld_override() was consulted only for Mach-O.
    let linker = linker_override.unwrap_or_else(|| "ld".into());
    if opts.verbose {
        print_verbose_command_line(&linker, &args);
    }
    let pending_output = PendingExternalOutput::prepare(output, "linker")?;
    let result = Command::new(&linker)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run linker '{}': {}", linker, e))?;
    if !result.status.success() {
        return Err(format!(
            "linker failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    pending_output.verify("linker")
}

fn link_inputs(
    inputs: &[PathBuf],
    output: &Path,
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    if opts.target.object_format() == crate::target::ObjectFormat::Elf {
        return link_inputs_elf_with_bundled_runtime(inputs, output, opts, bundled_runtime);
    }
    if let Some(linker) = afs_ld_override() {
        return link_inputs_with_afs_ld(&linker, inputs, output, opts, bundled_runtime);
    }

    let runtime = find_runtime_lib(bundled_runtime)?;
    let sdk = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .map_err(|e| format!("cannot run xcrun: {}", e))?;
    let sysroot = String::from_utf8_lossy(&sdk.stdout).trim().to_string();

    let mut args: Vec<String> = vec!["-o".into(), output.to_string_lossy().into_owned()];
    for dir in &opts.library_search_paths {
        args.push(format!("-L{}", dir.display()));
    }
    for operand in resolve_link_operands(inputs, opts)? {
        match operand {
            LinkOperand::Input(path) => args.push(path.to_string_lossy().into_owned()),
            LinkOperand::Library(name) => args.push(format!("-l{name}")),
        }
    }
    args.extend([
        runtime.path().to_string_lossy().into_owned(),
        "-lSystem".into(),
        "-syslibroot".into(),
        sysroot,
        "-e".into(),
        "_main".into(),
    ]);
    if !opts.shared {
        // The Rust static runtime is packaged in coarse archive members. Let
        // Apple ld trim unused runtime surfaces from final executables.
        args.push("-dead_strip".into());
    }
    push_macho_tail_link_flags(&mut args, opts);

    if opts.verbose {
        print_verbose_command_line("ld", &args);
    }

    let pending_output = PendingExternalOutput::prepare(output, "linker")?;
    let ld_result = Command::new("ld")
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run linker: {}", e))?;

    if !ld_result.status.success() {
        let stderr = String::from_utf8_lossy(&ld_result.stderr);
        return Err(format!("linker failed:\n{}", stderr));
    }

    pending_output.verify("linker")
}

fn link_inputs_with_afs_ld(
    linker: &str,
    inputs: &[PathBuf],
    output: &Path,
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    if opts.static_link {
        return Err("AFS_LD override does not yet support static-link mode".into());
    }

    let runtime = find_runtime_lib(bundled_runtime)?;
    let libsystem_tbd = find_libsystem_tbd()?;
    let mut args: Vec<String> = vec!["-arch".into(), "arm64".into()];
    if opts.shared {
        args.push("-dylib".into());
    } else {
        args.extend(["-e".into(), "_main".into()]);
    }
    args.extend(["-o".into(), output.to_string_lossy().into_owned()]);
    for dir in &opts.library_search_paths {
        args.push("-L".into());
        args.push(dir.to_string_lossy().into_owned());
    }
    for operand in resolve_link_operands(inputs, opts)? {
        match operand {
            LinkOperand::Input(path) => args.push(path.to_string_lossy().into_owned()),
            LinkOperand::Library(name) => {
                args.push("-l".into());
                args.push(name);
            }
        }
    }
    args.push(runtime.path().to_string_lossy().into_owned());
    args.push(libsystem_tbd);
    push_afs_ld_tail_link_flags(&mut args, opts);

    if opts.verbose {
        print_verbose_command_line(linker, &args);
    }

    let pending_output = PendingExternalOutput::prepare(output, "linker")?;
    let link_result = Command::new(linker)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run linker: {}", e))?;

    if link_result.status.success() {
        pending_output.verify("linker")
    } else {
        Err(format!(
            "linker failed:\n{}",
            String::from_utf8_lossy(&link_result.stderr)
        ))
    }
}

/// Append Mach-O flags that do not participate in the ordered user operand
/// stream. Search paths and inputs/libraries are emitted before this point.
fn push_macho_tail_link_flags(args: &mut Vec<String>, opts: &Options) {
    for path in &opts.rpath {
        args.push("-rpath".into());
        args.push(path.to_string_lossy().into_owned());
    }
    args.extend(opts.extra_link_args.iter().cloned());
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

fn print_verbose_command_line(program: &str, args: &[String]) {
    if args.is_empty() {
        eprintln!("{}", program);
    } else {
        eprintln!("{} {}", program, args.join(" "));
    }
}

fn push_afs_ld_tail_link_flags(args: &mut Vec<String>, opts: &Options) {
    for path in &opts.rpath {
        args.push("-rpath".into());
        args.push(path.to_string_lossy().into_owned());
    }
    args.extend(opts.extra_link_args.iter().cloned());
}

/// ELF assembler routing (x14). `None` means the in-process afs-as
/// pipeline — the default. `Some(path)` means spawn `<path> --64 -o
/// obj asm`: AFS_AS_PATH names a substitute assembler, AFS_AS=0
/// (or false/no/off) falls back to the system `as`.
fn elf_assembler_override() -> Option<String> {
    if let Some(assembler) = env_override("AFS_AS_PATH") {
        return Some(assembler);
    }
    let enabled = env_override("AFS_AS")?;
    if matches!(
        enabled.as_str(),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF"
    ) {
        return Some("as".into());
    }
    None
}

fn afs_ld_override() -> Option<String> {
    if let Some(linker) = env_override("AFS_LD_PATH") {
        return Some(linker);
    }
    let enabled = env_override("AFS_LD")?;
    if matches!(
        enabled.as_str(),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF"
    ) {
        return None;
    }
    Some(resolve_sibling_tool("afs-ld"))
}

fn resolve_sibling_tool(name: &str) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    name.into()
}

fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Link multiple object files with the runtime to produce a binary.
fn link_multi(
    objs: &[PathBuf],
    output: &Path,
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    link_inputs(objs, output, opts, bundled_runtime)
}

/// Compile multiple Fortran source files while preserving the requested
/// terminal phase.
///
/// Preprocessing and syntax dumps run in command order because they stop
/// before module resolution. Later phases scan and topologically order module
/// dependencies. Linking jobs compile temporary objects and link them; other
/// terminal modes publish one natural output per source (or ordered stdout for
/// `-E`) and never enter the linker.
pub fn compile_multi(opts: &Options) -> Result<(), String> {
    compile_multi_with_bundled_runtime(opts, None)
}

fn compile_multi_with_bundled_runtime(
    opts: &Options,
    bundled_runtime: Option<&'static [u8]>,
) -> Result<(), String> {
    let mut all_inputs = vec![opts.input.clone()];
    all_inputs.extend(opts.extra_inputs.iter().cloned());

    let terminal_mode = TerminalMode::from_options(opts);
    if let (Some(mode), Some(_)) = (terminal_mode, opts.output.as_ref()) {
        return Err(format!(
            "-o cannot be used with {} and multiple input files",
            mode.flag()
        ));
    }
    let link_output = terminal_mode.is_none().then(|| {
        opts.output
            .clone()
            .unwrap_or_else(|| PathBuf::from("a.out"))
    });
    let collect_link_dependencies = link_output
        .as_deref()
        .and_then(|output| dependency_file_path(opts, output))
        .is_some();
    if let Some(output) = link_output.as_deref() {
        validate_dependency_file_destination(opts, output, &[])?;
    }

    // Partition into Fortran sources (to compile) and prebuilt link
    // artifacts (objects/archives to pass straight to the linker). gfortran
    // accepts them mixed on one command line, e.g. the unit-test rule
    // `fc test.f90 build/foo.o build/bar.o -o test`.
    let source_inputs: Vec<PathBuf> = all_inputs
        .iter()
        .filter(|p| classify_cli_input(p) == CliInputKind::FortranSource)
        .cloned()
        .collect();
    let terminal_outputs = terminal_mode.map(|mode| {
        source_inputs
            .iter()
            .map(|input| mode.output_for_input(input))
            .collect::<Vec<_>>()
    });
    if let Some(outputs) = terminal_outputs.as_deref() {
        validate_unique_terminal_outputs(&source_inputs, outputs)?;
    }

    // A terminal multi-input job owns one default output per source. Remove
    // those destinations before scanning or compiling so a failed input can
    // never leave an older artifact looking like the result of this command.
    if let Some(outputs) = terminal_outputs.as_deref() {
        for output in outputs.iter().flatten() {
            match fs::remove_file(output) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot remove stale output '{}': {}",
                        output.display(),
                        error
                    ));
                }
            }
        }
    }

    // Syntax-only terminal phases must not acquire semantic dependency
    // requirements merely because more than one input was supplied. Later
    // phases still need module producers before their consumers.
    let order: Vec<usize> = if terminal_mode.is_some_and(|mode| !mode.requires_dependency_order()) {
        (0..source_inputs.len()).collect()
    } else {
        let file_deps: Vec<dep_scan::FileDeps> = source_inputs
            .iter()
            .map(|path| {
                let source_form = source_form_for_input(opts, path);
                let pp_config = preproc_config_for_input(opts, path, source_form);
                dep_scan::scan_file(path, &pp_config)
            })
            .collect::<Result<Vec<_>, _>>()?;
        dep_scan::resolve_compilation_order(&file_deps)?
    };

    // Compile each file in order.
    let tmp_dir = if terminal_mode.is_some() {
        None
    } else {
        let dir = std::env::temp_dir().join(format!("afs_multi_{}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir: {}", e))?;
        Some(dir)
    };

    // Compilation may run in dependency order, but linker operands must remain
    // in command-line order. Keep each generated object in the slot belonging
    // to its source rather than appending in compilation order.
    let mut source_objects: Vec<Option<PathBuf>> = vec![None; source_inputs.len()];
    let mut source_includes =
        collect_link_dependencies.then(|| vec![Vec::new(); source_inputs.len()]);
    for &idx in &order {
        let src = &source_inputs[idx];
        let child_output = match terminal_outputs.as_ref() {
            Some(outputs) => outputs[idx].clone(),
            None => {
                let tmp_dir = tmp_dir.as_ref().expect("temp dir for multi-file link");
                Some(tmp_dir.join(format!("source_{}.o", idx)))
            }
        };

        // Preserve every compilation-affecting option. Only orchestration and
        // output ownership differ for a child job.
        let mut sub_opts = opts.clone();
        sub_opts.input = src.clone();
        sub_opts.extra_inputs.clear();
        sub_opts.output = child_output.clone();
        if let Some(mode) = terminal_mode {
            mode.configure_child(&mut sub_opts);
        } else {
            TerminalMode::Object.configure_child(&mut sub_opts);
        }
        sub_opts.module_search_paths = {
            let mut paths = opts.module_search_paths.clone();
            if let Some(tmp_dir) = tmp_dir.as_ref() {
                paths.push(tmp_dir.clone()); // find .amod from earlier compilations
            }
            paths
        };
        if let Some(source_includes) = source_includes.as_mut() {
            // The outer link job owns one durable dependency file. Child
            // objects are temporary implementation details and must not
            // publish rules targeting their temporary paths.
            sub_opts.emit_depfile = false;
            sub_opts.depfile = None;
            sub_opts.dep_targets.clear();
            sub_opts.depfile_phony = false;
            source_includes[idx] =
                compile_with_bundled_runtime_and_dependencies(&sub_opts, bundled_runtime)?;
        } else {
            compile_with_bundled_runtime(&sub_opts, bundled_runtime)?;
        }
        if terminal_mode.is_none() {
            let obj_path = child_output.expect("object path for multi-file link");
            source_objects[idx] = Some(obj_path);
        }
    }

    if terminal_mode.is_some() {
        return Ok(());
    }

    // Assemble the link list in original command order: each source becomes
    // its compiled object; prebuilt artifacts pass straight through. This
    // preserves the ordering callers rely on (objects before archives).
    let mut next_source = 0;
    let link_list: Vec<PathBuf> = all_inputs
        .iter()
        .map(|input| {
            if classify_cli_input(input) == CliInputKind::LinkArtifact {
                input.clone()
            } else {
                let object = source_objects[next_source]
                    .take()
                    .expect("every source must produce one link object");
                next_source += 1;
                object
            }
        })
        .collect();

    let mut seen_includes = std::collections::HashSet::new();
    let included_files: Vec<PathBuf> = source_includes
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .filter(|path| seen_includes.insert(path.clone()))
        .collect();

    // Link all object files.
    let output = link_output.expect("link output for multi-file link");
    prepare_dependency_file(opts, &output, &included_files)?;
    link_multi(&link_list, &output, opts, bundled_runtime)?;

    // Cleanup.
    if let Some(tmp_dir) = tmp_dir {
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    write_link_dependency_file(opts, &output, &source_inputs, &included_files)?;

    Ok(())
}

/// Find libarmfortas_rt.a in common locations.
fn find_runtime_lib(bundled_runtime: Option<&'static [u8]>) -> Result<RuntimeArchive, String> {
    // 1. $AFS_RUNTIME_PATH — the explicit override.  Accepts either
    //    a directory containing libarmfortas_rt.a or the archive
    //    path directly.
    if let Ok(env_path) = std::env::var("AFS_RUNTIME_PATH") {
        let p = PathBuf::from(&env_path);
        if p.is_dir() {
            let candidate = p.join("libarmfortas_rt.a");
            if candidate.exists() {
                return Ok(RuntimeArchive::external(candidate));
            }
        } else if p.exists() {
            return Ok(RuntimeArchive::external(p));
        }
    }

    // 2. A source workspace that owns the compiler executable. Development
    //    binaries under target/{debug,release} retain automatic runtime
    //    freshness without trusting the caller's current directory.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(workspace_root) = exe
            .parent()
            .and_then(|dir| find_source_workspace_from(&[dir.to_path_buf()]))
        {
            if let Some(runtime) = runtime_from_workspace(&workspace_root)? {
                return Ok(runtime);
            }
        }
    }

    // 3. Runtime installed with the compiler binary:
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
                return Ok(RuntimeArchive::external(candidate.clone()));
            }
        }
    }

    // 4. Runtime carried by the compiler binary. Cargo installs executable
    //    targets but has no data-file installation hook, so installed
    //    armfortas/afs binaries materialize their target-matched archive into
    //    a private temporary directory for the duration of the linker call.
    //    Compiler-owned runtimes must win over anything inferred from the
    //    caller's current directory.
    if let Some(bytes) = bundled_runtime {
        return materialize_bundled_runtime(bytes);
    }

    // 5. Verified current source workspace — only for programmatic
    //    development callers that do not carry a runtime. Installed binaries
    //    return from the compiler-owned sources above and never reach this
    //    caller-controlled fallback.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(workspace_root) = find_source_workspace_from(&[cwd]) {
            if let Some(runtime) = runtime_from_workspace(&workspace_root)? {
                return Ok(runtime);
            }
        }
    }

    // 6. Standard install locations.
    for fixed in &[
        "/usr/local/lib/libarmfortas_rt.a",
        "/usr/local/lib/armfortas/libarmfortas_rt.a",
        "/opt/homebrew/lib/libarmfortas_rt.a",
    ] {
        if Path::new(fixed).exists() {
            return Ok(RuntimeArchive::external(PathBuf::from(fixed)));
        }
    }

    Err("cannot find libarmfortas_rt.a. Searched: \
         $AFS_RUNTIME_PATH, next to the compiler binary, the compiler's \
         bundled runtime, a verified armfortas workspace, and /usr/local/lib. Build with \
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

fn runtime_from_workspace(workspace_root: &Path) -> Result<Option<RuntimeArchive>, String> {
    let profile = RuntimeProfile::current();
    if let Some(candidate) = fresh_runtime_lib(workspace_root, profile) {
        return Ok(Some(RuntimeArchive::external(candidate)));
    }
    maybe_refresh_runtime_lib(workspace_root, profile)?;
    let candidate = runtime_lib_candidate(workspace_root, profile);
    Ok(candidate
        .exists()
        .then(|| RuntimeArchive::external(candidate)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn pending_external_output_rejects_and_cleans_uncommitted_artifacts() {
        let id = NEXT_ATOMIC_WRITE_ID.fetch_add(1, Ordering::Relaxed);
        let output = std::env::temp_dir().join(format!(
            "armfortas_pending_external_output_{}_{}",
            std::process::id(),
            id
        ));
        fs::write(&output, b"stale").expect("cannot seed stale output");

        let pending =
            PendingExternalOutput::prepare(&output, "test tool").expect("cannot prepare output");
        assert!(!output.exists(), "prepare retained a stale output");

        fs::write(&output, b"").expect("cannot write empty output");
        let error = pending
            .verify("test tool")
            .expect_err("empty output must not commit");
        assert!(
            error.contains("non-empty regular output"),
            "unexpected empty-output diagnostic: {error}"
        );
        assert!(!output.exists(), "verify retained an empty output");

        let pending =
            PendingExternalOutput::prepare(&output, "test tool").expect("cannot prepare output");
        fs::write(&output, b"partial").expect("cannot write partial output");
        drop(pending);
        assert!(!output.exists(), "drop retained a partial output");

        let pending =
            PendingExternalOutput::prepare(&output, "test tool").expect("cannot prepare output");
        fs::write(&output, b"complete").expect("cannot write complete output");
        pending
            .verify("test tool")
            .expect("non-empty regular output must commit");
        assert_eq!(
            fs::read(&output).expect("cannot read committed output"),
            b"complete"
        );
        let _ = fs::remove_file(output);
    }

    #[test]
    fn normalizes_module_source_provenance() {
        let cwd = std::env::temp_dir().join("armfortas-provenance-root");

        assert_eq!(
            module_source_provenance_from_absolute(
                &normalize_path_lexically(&cwd.join("./src/../parent.f90")),
                &cwd,
            ),
            "parent.f90"
        );
        assert_eq!(
            module_source_provenance_from_absolute(&cwd.join("src/child.f90"), &cwd),
            "src/child.f90"
        );
        assert_eq!(
            module_source_provenance_from_absolute(&cwd.join("../outside/child.f90"), &cwd),
            "../outside/child.f90"
        );
        assert_eq!(
            module_source_provenance_from_absolute(Path::new("/unrelated/child.f90"), &cwd),
            "child.f90"
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_windows_module_source_provenance() {
        let cwd = Path::new(r"C:\work\project\build");
        assert_eq!(
            module_source_provenance_from_absolute(
                Path::new(r"c:\work\project\src\child.f90"),
                cwd,
            ),
            "../src/child.f90"
        );
        assert_eq!(
            module_source_provenance_from_absolute(Path::new(r"D:\src\child.f90"), cwd),
            "child.f90"
        );

        let unc_cwd = Path::new(r"\\server\share\project\build");
        assert_eq!(
            module_source_provenance_from_absolute(
                Path::new(r"\\SERVER\SHARE\project\src\child.f90"),
                unc_cwd,
            ),
            "../src/child.f90"
        );
        assert_eq!(
            module_source_provenance_from_absolute(
                Path::new(r"\\server\other\project\src\child.f90"),
                unc_cwd,
            ),
            "child.f90"
        );
    }

    #[test]
    fn parses_os_optimization_flag() {
        assert_eq!(OptLevel::parse_flag("Os"), Some(OptLevel::Os));
        assert_eq!(OptLevel::parse_flag("os"), Some(OptLevel::Os));
        assert_eq!(OptLevel::Os.as_flag(), "-Os");
        assert_eq!(OptLevel::Os.as_str(), "Os");
    }

    #[test]
    fn floating_point_contraction_is_ofast_only() {
        assert!(OptLevel::Ofast.fp_contract());
        for level in [
            OptLevel::O0,
            OptLevel::O1,
            OptLevel::O2,
            OptLevel::O3,
            OptLevel::Os,
        ] {
            assert!(!level.fp_contract(), "{} must stay strict", level.as_flag());
        }
    }

    #[test]
    fn options_from_args_accepts_os() {
        let args = vec!["-Os".to_string(), "hello.f90".to_string()];
        let opts = Options::from_args(&args).expect("driver should accept -Os");
        assert_eq!(opts.opt_level, OptLevel::Os);
        assert_eq!(opts.input, PathBuf::from("hello.f90"));
    }

    #[test]
    fn options_from_args_preserves_mt_and_mq_target_kinds_and_order() {
        let args = vec![
            "-MT".to_string(),
            "raw separated".to_string(),
            "-MQ".to_string(),
            "quoted separated".to_string(),
            "-MTraw-attached".to_string(),
            "-MQquoted-attached".to_string(),
            "hello.f90".to_string(),
        ];
        let opts = Options::from_args(&args).expect("driver should accept -MT and -MQ targets");
        assert_eq!(
            opts.dep_targets,
            vec![
                DepTarget::Verbatim("raw separated".to_string()),
                DepTarget::Quoted("quoted separated".to_string()),
                DepTarget::Verbatim("raw-attached".to_string()),
                DepTarget::Quoted("quoted-attached".to_string()),
            ]
        );
    }

    #[test]
    fn default_target_is_host() {
        assert_eq!(Options::default().target, crate::target::TargetSpec::host());
    }

    #[test]
    fn options_from_args_accepts_target_flag() {
        for args in [
            vec![
                "--target".to_string(),
                "x86_64-freebsd".to_string(),
                "hello.f90".to_string(),
            ],
            vec![
                "--target=x86_64-freebsd".to_string(),
                "hello.f90".to_string(),
            ],
        ] {
            let opts = Options::from_args(&args).expect("driver should accept --target");
            assert_eq!(opts.target.triple(), "x86_64-freebsd");
        }
    }

    #[test]
    fn options_from_args_rejects_unknown_target() {
        let args = vec![
            "--target=riscv64-linux".to_string(),
            "hello.f90".to_string(),
        ];
        let err = Options::from_args(&args)
            .err()
            .expect("unknown target must be rejected");
        assert!(
            err.contains("supported targets: arm64-macos"),
            "diagnostic must list supported targets, got: {}",
            err
        );
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
    fn parse_cli_accepts_fpm_gnu_debug_probe_flags() {
        let args = vec![
            "-fmax-errors=1".to_string(),
            "-fcheck=array-temps".to_string(),
            "-fcoarray=single".to_string(),
            "-Werror=implicit-interface".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) =
            parse_cli(&args).expect("driver should accept fpm GNU debug probe flags")
        else {
            panic!("expected compile options");
        };
        assert_eq!(opts.max_errors_compat, Some(1));
        assert!(opts.check_array_temps_compat);
        assert!(opts.coarray_single_compat);
        assert!(opts.werror_implicit_interface_compat);
        assert!(
            !opts.warn_as_error,
            "-Werror=implicit-interface should not promote every compatibility warning"
        );
        for needle in [
            "-fmax-errors is recognized",
            "-fcheck=array-temps is accepted for compatibility",
            "-fcoarray=single is accepted for compatibility",
            "-Werror=implicit-interface is accepted for compatibility",
        ] {
            assert!(
                opts.cli_warnings
                    .iter()
                    .any(|warning| warning.contains(needle)),
                "missing warning `{}` in {:?}",
                needle,
                opts.cli_warnings
            );
        }
    }

    #[test]
    fn parse_cli_rejects_unknown_werror_warning_names_as_errors() {
        let args = vec!["-Werror=unknown-flag".to_string(), "hello.f90".to_string()];
        let ParsedCli::Compile(opts) =
            parse_cli(&args).expect("driver should parse unknown -Werror warning names")
        else {
            panic!("expected compile options");
        };
        assert!(opts.warn_as_error);
        assert!(
            opts.cli_warnings
                .iter()
                .any(|warning| warning.contains("unrecognized warning option")),
            "expected unknown warning option diagnostic, got {:?}",
            opts.cli_warnings
        );
    }

    #[test]
    fn parse_cli_warning_controls_honor_last_option_and_global_suppression() {
        let disabled_last = vec![
            "-Wall".to_string(),
            "-Werror".to_string(),
            "-Wno-all".to_string(),
            "-Wno-error".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) =
            parse_cli(&disabled_last).expect("driver should parse warning suppressions")
        else {
            panic!("expected compile options");
        };
        assert!(!opts.warn_all);
        assert!(!opts.warn_as_error);
        assert!(opts.cli_warnings.is_empty());

        let enabled_last = vec![
            "-Wno-all".to_string(),
            "-Wno-error".to_string(),
            "-Wall".to_string(),
            "-Werror".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) =
            parse_cli(&enabled_last).expect("driver should re-enable warning controls")
        else {
            panic!("expected compile options");
        };
        assert!(opts.warn_all);
        assert!(opts.warn_as_error);
        assert_eq!(opts.cli_warnings.len(), 1);

        for args in [
            vec![
                "-w".to_string(),
                "-Wall".to_string(),
                "hello.f90".to_string(),
            ],
            vec![
                "-Wall".to_string(),
                "-w".to_string(),
                "hello.f90".to_string(),
            ],
        ] {
            let ParsedCli::Compile(opts) =
                parse_cli(&args).expect("driver should parse global warning suppression")
            else {
                panic!("expected compile options");
            };
            assert!(opts.suppress_warnings);
            assert!(opts.cli_warnings.is_empty());
        }
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
    fn parse_cli_accepts_numeric_ffree_line_length_flag() {
        let args = vec![
            "-ffree-line-length-132".to_string(),
            "hello.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) =
            parse_cli(&args).expect("driver should accept -ffree-line-length-132")
        else {
            panic!("expected compile options");
        };
        assert_eq!(opts.free_line_length_limit, Some(132));
        assert!(!opts.free_line_length_none_compat);
        assert!(opts.cli_warnings.is_empty());
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

    #[test]
    fn parse_cli_preserves_input_and_library_operand_order() {
        let args = vec![
            "main.o".to_string(),
            "-lprovider".to_string(),
            "consumer.o".to_string(),
            "-l".to_string(),
            "tail".to_string(),
        ];
        let ParsedCli::Compile(opts) = parse_cli(&args).expect("driver should parse link operands")
        else {
            panic!("expected compile options");
        };
        assert_eq!(opts.input, PathBuf::from("main.o"));
        assert_eq!(opts.extra_inputs, vec![PathBuf::from("consumer.o")]);
        assert_eq!(
            opts.link_operands,
            vec![
                LinkOperand::Input(PathBuf::from("main.o")),
                LinkOperand::Library("provider".to_string()),
                LinkOperand::Input(PathBuf::from("consumer.o")),
                LinkOperand::Library("tail".to_string()),
            ]
        );
    }

    #[test]
    fn resolve_link_operands_substitutes_compiled_sources_without_reordering_libraries() {
        let args = vec![
            "consumer.f90".to_string(),
            "-lprovider".to_string(),
            "main.f90".to_string(),
        ];
        let ParsedCli::Compile(opts) = parse_cli(&args).expect("driver should parse link operands")
        else {
            panic!("expected compile options");
        };

        assert_eq!(
            resolve_link_operands(
                &[
                    PathBuf::from("/tmp/consumer.o"),
                    PathBuf::from("/tmp/main.o"),
                ],
                &opts,
            )
            .expect("compiled sources should fill their original linker slots"),
            vec![
                LinkOperand::Input(PathBuf::from("/tmp/consumer.o")),
                LinkOperand::Library("provider".to_string()),
                LinkOperand::Input(PathBuf::from("/tmp/main.o")),
            ]
        );

        let mismatch =
            resolve_link_operands(&[PathBuf::from("/tmp/consumer.o")], &opts).unwrap_err();
        assert!(
            mismatch.contains("2 input slots for 1 resolved inputs"),
            "malformed programmatic options must fail loudly: {mismatch}"
        );
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

    /// The five backend_allows_* tests assert ARM64 instruction shapes;
    /// since x00 they say so instead of assuming the host is ARM64.
    fn arm64_macos() -> crate::target::TargetSpec {
        crate::target::TargetSpec::parse("arm64-macos").unwrap()
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
            target: arm64_macos(),
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
            target: arm64_macos(),
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
            target: arm64_macos(),
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
            target: arm64_macos(),
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
            target: arm64_macos(),
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
            crate::codegen::mir::MachineFunction::new("bump".into()),
            crate::codegen::mir::MachineFunction::new("__prog_audit_entry".into()),
        ];

        assert_eq!(
            crate::codegen::arm64::main_wrapper_target(&allocated),
            Some("__prog_audit_entry"),
            "main wrapper should call the lowered program body, not the first helper"
        );
    }
}
