//! End-to-end test harness for ARMFORTAS.
//!
//! Discovers Fortran source files in `test_programs/`, compiles each with
//! `armfortas`, runs the binary, and checks stdout against `! CHECK:`
//! annotations.
//!
//! Each `! CHECK:` line specifies a substring that must appear in the output,
//! in order. Whitespace is trimmed for comparison.
//!
//! ## XFAIL annotations
//!
//! A program may carry one or more `! XFAIL: <reason>` lines anywhere in
//! the source. An XFAIL'd program is *expected* to fail at compile or
//! runtime (or to mismatch its CHECKs) — it's tracking a known bug. The
//! harness reports:
//!
//!   * `XFAIL`  — the program failed as expected. Counted as a pass.
//!   * `XPASS`  — the program unexpectedly succeeded. Counted as a
//!     failure so we get loud notification that the bug is now fixed
//!     and the XFAIL annotation should be removed.
//!
//! XFAIL'd programs are how we capture audit findings as living
//! regression tests *before* the underlying bug is fixed. Each finding
//! gets a program in `test_programs/` whose annotation references the
//! audit ID (`! XFAIL: audit BLOCKING-1 (implied-do negative step)`),
//! so a future audit can grep `test_programs/` for the finding ID and
//! immediately see whether the bug class is covered.
//!
//! ## ERROR_EXPECTED annotations
//!
//! A program may also carry an `! ERROR_EXPECTED: <substring>` line.
//! That asserts the program **must fail to compile**, and the
//! compiler's stderr **must contain** the given substring. This is
//! how we test "should be a diagnostic" cases — Fortran constraint
//! violations that the compiler is required to reject. The semantics:
//!
//!   * If `ERROR_EXPECTED` is present, CHECK lines are ignored.
//!   * If compilation succeeds, the test fails.
//!   * If compilation fails but stderr doesn't contain the substring,
//!     the test fails.
//!   * If compilation fails with the expected substring, the test
//!     passes.
//!
//! `ERROR_EXPECTED` composes with `XFAIL`: if a program is annotated
//! with both, an XFAIL fires when the expected error is **not**
//! produced (the bug is "we don't yet diagnose this"), and an XPASS
//! fires once we start diagnosing it correctly (so the XFAIL
//! annotation can come off and the program becomes a regular
//! "diagnostic regression" test).
//!
//! `! ERROR_SPAN: <line>:<col>` composes with `ERROR_EXPECTED` and
//! asserts that the diagnostic points at the expected source
//! location. The span check is substring-based against the emitted
//! diagnostic location text, so the compiler can print either
//! `path:line:col:` or `line:col:` and still satisfy the contract.
//!
//! ## IR_CHECK / IR_NOT annotations
//!
//! For tests that need to assert on the *shape* of the lowered IR
//! (not just the runtime answer), two extra annotations exist:
//!
//!   * `! IR_CHECK: <substring>` — the substring must appear in the
//!     compiler's `--emit-ir` output. Multiple IR_CHECKs must appear
//!     in the order they're declared.
//!   * `! IR_NOT: <substring>` — the substring must NOT appear in the
//!     `--emit-ir` output. Used for negative-shape assertions like
//!     "this PARAMETER local must not have a `store` instruction"
//!     or "this expression must not lower to a `global_addr`".
//!
//! IR shape is only stable at -O0 (the optimization passes erase
//! dead code, fold constants, hoist loads, etc.), so IR_CHECK /
//! IR_NOT only fire at the -O0 test level. The runtime CHECKs
//! continue to run at every opt level as before.
//!
//! Audit5 MIN-2: this exists because audit4 captured the
//! parameter-inlining and module-allocatable bugs as runtime tests
//! only. A future regression that broke the IR shape but happened
//! to land on the right runtime answer would slip through.
//!
//! ## STDERR_CHECK / EXIT_CODE annotations
//!
//! Runtime tests can also assert on stderr and process exit status:
//!
//!   * `! STDERR_CHECK: <substring>` — ordered substring checks
//!     against the program's stderr stream.
//!   * `! EXIT_CODE: <int>` — exact process exit code. Without this
//!     annotation, the harness preserves the old rule that runtime
//!     tests must exit successfully.
//!
//! This makes runtime tests expressive enough for paths like
//! `ERROR STOP`, warning-like stderr output, and future
//! side-effect-heavy programs without forcing them through
//! `ERROR_EXPECTED`, which is compile-failure-only.
//!
//! ## ASM_CHECK / ASM_NOT annotations
//!
//! Runtime tests can also pin emitted assembly shape:
//!
//!   * `! ASM_CHECK: <substring>` — the substring must appear in
//!     the compiler's `-S` output. Multiple checks must appear in
//!     the order they are declared.
//!   * `! ASM_NOT: <substring>` — the substring must NOT appear in
//!     the emitted assembly text.
//!
//! Unlike IR checks, assembly shape can legitimately vary by opt
//! level, so ASM checks fire at every optimization level. Tests
//! should use stable substrings that are intentionally expected
//! across the requested matrix.
//!
//! ## FILE_CHECK / FILE_NOT / FILE_EXISTS / FILE_MISSING / FILE_LINE_COUNT / FILE_RERUN_MODE / FILE_SET_EXACT annotations
//!
//! Runtime tests can assert on files created inside their per-test
//! sandbox:
//!
//!   * `! FILE_CHECK: <relative-path> => <substring>` — the file must
//!     exist after execution, and the substring must appear in its
//!     contents. Multiple checks for the same file must appear in the
//!     order declared.
//!   * `! FILE_NOT: <relative-path> => <substring>` — the file must
//!     exist, and the substring must not appear in its contents.
//!   * `! FILE_EXISTS: <relative-path>` — the file must exist after the
//!     run, regardless of content.
//!   * `! FILE_MISSING: <relative-path>` — the file must not exist after
//!     the run.
//!   * `! FILE_LINE_COUNT: <relative-path> => <int>` — the file must
//!     exist and contain exactly that many text lines.
//!   * `! FILE_RERUN_MODE: <relative-path> => stable|append` — when the
//!     program is executed twice in the same sandbox, the named file must
//!     either be byte-identical after both runs (`stable`) or grow by
//!     strict append (`append`).
//!   * `! FILE_SET_EXACT: <relative-path>[,<relative-path>...]` — the
//!     final sandbox file set must match exactly, with no extra side
//!     effects beyond the listed relative paths.
//!
//! Paths are sandbox-relative on purpose. The harness runs each binary
//! in a private temp directory, so file assertions pin side effects
//! without letting tests stomp on one another.
//!
//! ## REPRO_CHECK annotations
//!
//! Tests can also opt into explicit reproducibility checks:
//!
//!   * `! REPRO_CHECK: asm` — compile twice with `-S` and require
//!     identical assembly bytes.
//!   * `! REPRO_CHECK: obj` — compile twice with `-c` and require
//!     identical object bytes.
//!   * `! REPRO_CHECK: run` — execute twice in fresh sandboxes and
//!     require identical exit/stdout/stderr plus identical written files.
//!   * `! REPRO_CHECK: run_same_sandbox` — execute twice in the same
//!     sandbox and require the second run to leave the same final
//!     exit/stdout/stderr and file snapshot as the first.
//!
//! These are test-local determinism assertions, lighter-weight than the
//! dedicated global determinism tests at the bottom of this file.
//!
//! ## OPT_EQ annotations
//!
//! Cross-opt invariants can be asserted explicitly:
//!
//!   * `! OPT_EQ: O0,O1,O2 => stdout|stderr|exit`
//!   * `! OPT_EQ: O0,O1 => asm`
//!
//! The first listed opt level is the baseline. When that level runs, the
//! harness compiles the same source at the other listed opt levels and
//! compares the requested surfaces. This lets a test say "these runtime
//! surfaces must agree across optimization" without also pinning every
//! assembly detail at every level.
//!
//! ## PHASE_TRIANGULATE annotations
//!
//! Phase triangulation lets a runtime test say that additional compiler
//! surfaces must also materialize successfully at the same optimization level:
//!
//!   * `! PHASE_TRIANGULATE: ir|asm|obj`
//!   * `! PHASE_TRIANGULATE: ir|asm|obj|clean`
//!   * `! PHASE_TRIANGULATE: ir|asm|obj|repro`
//!
//! The linked runtime path is the anchor. If the program runs correctly but
//! one of the requested extra surfaces fails to compile or produces an empty
//! artifact, the test still fails. This is how the harness starts relating
//! linked execution to `--emit-ir`, `-S`, and `-c` instead of treating them as
//! isolated one-off checks.
//!
//! `clean` strengthens that oracle further: the compile-only phases must leave
//! only their explicit output artifact in a private phase sandbox. That lets a
//! runtime-side-effecting program assert that `--emit-ir`, `-S`, and `-c` do
//! not accidentally create the files that only linked execution should create.
//!
//! `repro` strengthens it in a different direction: each requested compile-only
//! phase must produce byte-identical output across two independent compilations.
//! This keeps pipeline oracles from checking only "exists" when what we really
//! need is "exists, stays clean, and stays deterministic".

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

/// A single expected check.
struct Check {
    line_num: usize,
    pattern: String,
}

/// A single file-content assertion against the per-test sandbox.
struct FileCheck {
    line_num: usize,
    rel_path: String,
    pattern: String,
    negative: bool,
}

struct FilePresenceCheck {
    line_num: usize,
    rel_path: String,
    should_exist: bool,
}

struct FileLineCountCheck {
    line_num: usize,
    rel_path: String,
    expected_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileRerunMode {
    Stable,
    Append,
}

struct FileRerunModeCheck {
    line_num: usize,
    rel_path: String,
    mode: FileRerunMode,
}

struct FileSetExactCheck {
    line_num: usize,
    rel_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReproStage {
    Asm,
    Obj,
    Run,
    RunSameSandbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptEqComponent {
    Stdout,
    Stderr,
    Exit,
    Asm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OptEqRule {
    line_num: usize,
    opt_flags: Vec<String>,
    components: Vec<OptEqComponent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseSurface {
    Ir,
    Asm,
    Obj,
    Clean,
    Repro,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhaseTriangulation {
    line_num: usize,
    surfaces: Vec<PhaseSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunSnapshot {
    exit_code: i32,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, Vec<u8>>,
}

// ---- Multi-file test support ----

/// A named source segment extracted from a multi-file test program.
struct MultifileSegment {
    /// The declared filename (e.g. "mymod.f90").
    name: String,
    /// The Fortran source for this segment.
    source: String,
}

/// Split a source file on `!--- file: <name>` markers.
/// Returns `None` if no markers are present (single-file test).
fn split_multifile_segments(source: &str) -> Option<Vec<MultifileSegment>> {
    let mut segments = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("!--- file:") {
            // Flush previous segment.
            if let Some(name) = current_name.take() {
                segments.push(MultifileSegment {
                    name,
                    source: current_lines.join("\n") + "\n",
                });
                current_lines.clear();
            }
            current_name = Some(rest.trim().to_string());
        } else if current_name.is_some() {
            current_lines.push(line);
        }
        // Lines before any !--- file: marker are annotation-only preamble
        // (CHECK, MULTIFILE_LINK, etc.) — not included in any segment.
    }

    // Flush last segment.
    if let Some(name) = current_name {
        segments.push(MultifileSegment {
            name,
            source: current_lines.join("\n") + "\n",
        });
    }

    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

/// Extract `! MULTIFILE_LINK: a.f90 b.f90 ...` — the link order.
/// If absent, segments are linked in declaration order.
fn extract_multifile_link(source: &str) -> Option<Vec<String>> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("! MULTIFILE_LINK:") {
            let names: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
            if !names.is_empty() {
                return Some(names);
            }
        }
    }
    None
}

fn candidate_target_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors() {
            let Some(name) = ancestor.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if matches!(name, "debug" | "release") {
                dirs.push(ancestor.to_path_buf());
                break;
            }
        }
    }
    for dir in ["target/release", "target/debug"] {
        let candidate = PathBuf::from(dir);
        if !dirs.iter().any(|existing| existing == &candidate) {
            dirs.push(candidate);
        }
    }
    dirs
}

/// Find the static runtime library.
fn find_runtime() -> PathBuf {
    for dir in candidate_target_dirs() {
        let p = dir.join("libarmfortas_rt.a");
        if p.exists() {
            return p;
        }
    }
    panic!("libarmfortas_rt.a not found — run `cargo build` first");
}

/// Get the macOS SDK path for linking.
fn sdk_path() -> String {
    let out = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("xcrun failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Compile a single .f90 to .o with -c.
fn compile_to_object(
    compiler: &Path,
    source: &Path,
    output: &Path,
    opt_flag: &str,
    search_dir: &Path,
) -> Result<(), String> {
    let result = Command::new(compiler)
        .current_dir(search_dir)
        .args([
            source.to_str().unwrap(),
            "-c",
            opt_flag,
            "-o",
            output.to_str().unwrap(),
            &format!("-I{}", search_dir.display()),
        ])
        .output()
        .map_err(|e| format!("cannot launch compiler for {}: {}", source.display(), e))?;
    if !result.status.success() {
        return Err(format!(
            "compile {} failed:\n{}",
            source.display(),
            String::from_utf8_lossy(&result.stderr),
        ));
    }
    Ok(())
}

/// Link object files with the runtime into a binary.
fn link_objects(objects: &[PathBuf], output: &Path) -> Result<(), String> {
    let runtime = find_runtime();
    let sdk = sdk_path();
    let mut args: Vec<String> = vec!["-o".into(), output.to_str().unwrap().into()];
    for o in objects {
        args.push(o.to_str().unwrap().into());
    }
    args.push(runtime.to_str().unwrap().into());
    args.extend([
        "-lSystem".into(),
        "-syslibroot".into(),
        sdk,
        "-arch".into(),
        "arm64".into(),
    ]);
    let result = Command::new("ld")
        .args(&args)
        .output()
        .map_err(|e| format!("cannot launch linker: {}", e))?;
    if !result.status.success() {
        return Err(format!(
            "link failed:\n{}",
            String::from_utf8_lossy(&result.stderr),
        ));
    }
    Ok(())
}

fn unique_temp_path(prefix: &str, stem: &str, tag: &str, ext: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "afs_{}_{}_{}_{}_{}{}",
        prefix,
        std::process::id(),
        id,
        stem,
        tag.trim_start_matches('-'),
        ext
    ))
}

/// Extract ordered substring checks from a Fortran source file.
fn extract_prefixed_checks(source: &str, prefix: &str) -> Vec<Check> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                Some(Check {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Extract `! CHECK:` patterns from a Fortran source file.
fn extract_checks(source: &str) -> Vec<Check> {
    extract_prefixed_checks(source, "! CHECK:")
}

/// Extract `! STDERR_CHECK:` patterns from a Fortran source file.
fn extract_stderr_checks(source: &str) -> Vec<Check> {
    extract_prefixed_checks(source, "! STDERR_CHECK:")
}

/// Extract `! XFAIL:` reason text. Returns the first reason found, or
/// `None` if the program has no XFAIL annotation. Multiple XFAIL lines
/// are allowed (only the first is reported); a typical pattern is one
/// audit ID per line for findings of the same class.
fn extract_xfail(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("! XFAIL:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Extract `! ERROR_EXPECTED:` substring text. Returns the expected
/// stderr substring if any. Programs with this annotation are
/// asserted to fail compilation.
fn extract_error_expected(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("! ERROR_EXPECTED:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpectedSpan {
    line_num: usize,
    line: usize,
    col: usize,
}

/// Extract `! ERROR_SPAN:` and parse it as an exact line:col pair.
fn extract_error_span(source: &str, filename: &str) -> Result<Option<ExpectedSpan>, String> {
    let mut matches = source.lines().enumerate().filter_map(|(i, line)| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("! ERROR_SPAN:")
            .map(|rest| (i + 1, rest.trim()))
    });

    let Some((line_num, raw)) = matches.next() else {
        return Ok(None);
    };

    if let Some((extra_line, _)) = matches.next() {
        return Err(format!(
            "{}:{}: multiple ERROR_SPAN annotations are not allowed (another at line {})",
            filename, line_num, extra_line
        ));
    }

    let Some((line, col)) = raw.split_once(':') else {
        return Err(format!(
            "{}:{}: ERROR_SPAN must be written as <line>:<col>, got '{}'",
            filename, line_num, raw
        ));
    };

    let line = line.parse::<usize>().map_err(|_| {
        format!(
            "{}:{}: ERROR_SPAN line must be a decimal integer, got '{}'",
            filename, line_num, line
        )
    })?;
    let col = col.parse::<usize>().map_err(|_| {
        format!(
            "{}:{}: ERROR_SPAN column must be a decimal integer, got '{}'",
            filename, line_num, col
        )
    })?;

    Ok(Some(ExpectedSpan {
        line_num,
        line,
        col,
    }))
}

/// Extract `! EXIT_CODE:` and parse it as an exact expected exit
/// status. Multiple annotations are rejected as a test setup error
/// so the expected runtime contract stays unambiguous.
fn extract_exit_code(source: &str, filename: &str) -> Result<Option<i32>, String> {
    let mut matches = source.lines().enumerate().filter_map(|(i, line)| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("! EXIT_CODE:")
            .map(|rest| (i + 1, rest.trim()))
    });

    let Some((line_num, raw)) = matches.next() else {
        return Ok(None);
    };

    if let Some((extra_line, _)) = matches.next() {
        return Err(format!(
            "{}:{}: multiple EXIT_CODE annotations are not allowed (another at line {})",
            filename, line_num, extra_line
        ));
    }

    raw.parse::<i32>().map(Some).map_err(|_| {
        format!(
            "{}:{}: EXIT_CODE must be a decimal integer, got '{}'",
            filename, line_num, raw
        )
    })
}

fn extract_file_checks(
    source: &str,
    filename: &str,
    pos_prefix: &str,
    neg_prefix: &str,
) -> Result<Vec<FileCheck>, String> {
    let mut checks = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let (rest, negative) = if let Some(rest) = trimmed.strip_prefix(pos_prefix) {
            (rest.trim(), false)
        } else if let Some(rest) = trimmed.strip_prefix(neg_prefix) {
            (rest.trim(), true)
        } else {
            continue;
        };

        let Some((raw_path, raw_pattern)) = rest.split_once("=>") else {
            return Err(format!(
                "{}:{}: {} must be written as <relative-path> => <substring>",
                filename,
                i + 1,
                if negative { "FILE_NOT" } else { "FILE_CHECK" }
            ));
        };

        let rel_path = raw_path.trim();
        if rel_path.is_empty() {
            return Err(format!(
                "{}:{}: FILE_CHECK/FILE_NOT path cannot be empty",
                filename,
                i + 1
            ));
        }
        if Path::new(rel_path).is_absolute() {
            return Err(format!(
                "{}:{}: FILE_CHECK/FILE_NOT path must be relative, got '{}'",
                filename,
                i + 1,
                rel_path
            ));
        }

        checks.push(FileCheck {
            line_num: i + 1,
            rel_path: rel_path.to_string(),
            pattern: raw_pattern.trim().to_string(),
            negative,
        });
    }
    Ok(checks)
}

fn extract_file_presence_checks(
    source: &str,
    filename: &str,
) -> Result<Vec<FilePresenceCheck>, String> {
    let mut checks = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let (rest, should_exist, directive_name) =
            if let Some(rest) = trimmed.strip_prefix("! FILE_EXISTS:") {
                (rest.trim(), true, "FILE_EXISTS")
            } else if let Some(rest) = trimmed.strip_prefix("! FILE_MISSING:") {
                (rest.trim(), false, "FILE_MISSING")
            } else {
                continue;
            };

        if rest.is_empty() {
            return Err(format!(
                "{}:{}: {} path cannot be empty",
                filename,
                i + 1,
                directive_name
            ));
        }
        if Path::new(rest).is_absolute() {
            return Err(format!(
                "{}:{}: {} path must be relative, got '{}'",
                filename,
                i + 1,
                directive_name,
                rest
            ));
        }

        checks.push(FilePresenceCheck {
            line_num: i + 1,
            rel_path: rest.to_string(),
            should_exist,
        });
    }
    Ok(checks)
}

fn extract_file_line_count_checks(
    source: &str,
    filename: &str,
) -> Result<Vec<FileLineCountCheck>, String> {
    let mut checks = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! FILE_LINE_COUNT:") else {
            continue;
        };
        let Some((raw_path, raw_count)) = rest.trim().split_once("=>") else {
            return Err(format!(
                "{}:{}: FILE_LINE_COUNT must be written as <relative-path> => <int>",
                filename,
                i + 1
            ));
        };

        let rel_path = raw_path.trim();
        if rel_path.is_empty() {
            return Err(format!(
                "{}:{}: FILE_LINE_COUNT path cannot be empty",
                filename,
                i + 1
            ));
        }
        if Path::new(rel_path).is_absolute() {
            return Err(format!(
                "{}:{}: FILE_LINE_COUNT path must be relative, got '{}'",
                filename,
                i + 1,
                rel_path
            ));
        }

        let expected_lines = raw_count.trim().parse::<usize>().map_err(|_| {
            format!(
                "{}:{}: FILE_LINE_COUNT line count must be a non-negative integer, got '{}'",
                filename,
                i + 1,
                raw_count.trim()
            )
        })?;

        checks.push(FileLineCountCheck {
            line_num: i + 1,
            rel_path: rel_path.to_string(),
            expected_lines,
        });
    }
    Ok(checks)
}

fn extract_file_rerun_mode_checks(
    source: &str,
    filename: &str,
) -> Result<Vec<FileRerunModeCheck>, String> {
    let mut checks = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! FILE_RERUN_MODE:") else {
            continue;
        };
        let Some((raw_path, raw_mode)) = rest.trim().split_once("=>") else {
            return Err(format!(
                "{}:{}: FILE_RERUN_MODE must be written as <relative-path> => stable|append",
                filename,
                i + 1
            ));
        };

        let rel_path = raw_path.trim();
        if rel_path.is_empty() {
            return Err(format!(
                "{}:{}: FILE_RERUN_MODE path cannot be empty",
                filename,
                i + 1
            ));
        }
        if Path::new(rel_path).is_absolute() {
            return Err(format!(
                "{}:{}: FILE_RERUN_MODE path must be relative, got '{}'",
                filename,
                i + 1,
                rel_path
            ));
        }

        let mode = match raw_mode.trim() {
            "stable" => FileRerunMode::Stable,
            "append" => FileRerunMode::Append,
            other => {
                return Err(format!(
                    "{}:{}: FILE_RERUN_MODE must be stable or append; got '{}'",
                    filename,
                    i + 1,
                    other
                ));
            }
        };

        checks.push(FileRerunModeCheck {
            line_num: i + 1,
            rel_path: rel_path.to_string(),
            mode,
        });
    }
    Ok(checks)
}

fn extract_file_set_exact(
    source: &str,
    filename: &str,
) -> Result<Option<FileSetExactCheck>, String> {
    let mut found: Option<FileSetExactCheck> = None;
    for (i, line) in source.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! FILE_SET_EXACT:") else {
            continue;
        };
        if let Some(existing) = &found {
            return Err(format!(
                "{}:{}: multiple FILE_SET_EXACT annotations are not allowed (another at line {})",
                filename, line_num, existing.line_num
            ));
        }

        let mut rel_paths = Vec::new();
        for token in rest.trim().split(',') {
            let rel_path = token.trim();
            if rel_path.is_empty() {
                return Err(format!(
                    "{}:{}: FILE_SET_EXACT paths cannot be empty",
                    filename, line_num
                ));
            }
            if Path::new(rel_path).is_absolute() {
                return Err(format!(
                    "{}:{}: FILE_SET_EXACT paths must be relative, got '{}'",
                    filename, line_num, rel_path
                ));
            }
            if !rel_paths
                .iter()
                .any(|existing: &String| existing == rel_path)
            {
                rel_paths.push(rel_path.to_string());
            }
        }
        if rel_paths.is_empty() {
            return Err(format!(
                "{}:{}: FILE_SET_EXACT needs at least one relative path",
                filename, line_num
            ));
        }
        rel_paths.sort();

        found = Some(FileSetExactCheck {
            line_num,
            rel_paths,
        });
    }
    Ok(found)
}

fn extract_repro_checks(source: &str, filename: &str) -> Result<Vec<ReproStage>, String> {
    let mut stages = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! REPRO_CHECK:") else {
            continue;
        };
        let stage = match rest.trim() {
            "asm" => ReproStage::Asm,
            "obj" => ReproStage::Obj,
            "run" => ReproStage::Run,
            "run_same_sandbox" => ReproStage::RunSameSandbox,
            other => {
                return Err(format!(
                    "{}:{}: REPRO_CHECK must be one of asm, obj, run, run_same_sandbox; got '{}'",
                    filename,
                    i + 1,
                    other
                ));
            }
        };
        if !stages.contains(&stage) {
            stages.push(stage);
        }
    }
    Ok(stages)
}

fn parse_supported_opt(token: &str, filename: &str, line_num: usize) -> Result<String, String> {
    let trimmed = token.trim().trim_start_matches('-');
    match trimmed {
        "O0" | "O1" | "O2" | "O3" | "Os" | "Ofast" => Ok(format!("-{}", trimmed)),
        other => Err(format!(
            "{}:{}: OPT_EQ only supports O0, O1, O2, O3, Os, or Ofast; got '{}'",
            filename, line_num, other
        )),
    }
}

fn extract_opt_eq_rules(source: &str, filename: &str) -> Result<Vec<OptEqRule>, String> {
    let mut rules = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! OPT_EQ:") else {
            continue;
        };
        let Some((opts_text, components_text)) = rest.trim().split_once("=>") else {
            return Err(format!(
                "{}:{}: OPT_EQ must be written as <opts> => <components>",
                filename, line_num
            ));
        };

        let opt_flags = opts_text
            .split(',')
            .map(|token| parse_supported_opt(token, filename, line_num))
            .collect::<Result<Vec<_>, _>>()?;
        if opt_flags.len() < 2 {
            return Err(format!(
                "{}:{}: OPT_EQ needs at least two opt levels to compare",
                filename, line_num
            ));
        }

        let mut components = Vec::new();
        for token in components_text.split('|') {
            let component = match token.trim() {
                "stdout" => OptEqComponent::Stdout,
                "stderr" => OptEqComponent::Stderr,
                "exit" => OptEqComponent::Exit,
                "asm" => OptEqComponent::Asm,
                other => {
                    return Err(format!(
                        "{}:{}: OPT_EQ components must be stdout, stderr, exit, or asm; got '{}'",
                        filename, line_num, other
                    ))
                }
            };
            components.push(component);
        }
        if components.is_empty() {
            return Err(format!(
                "{}:{}: OPT_EQ needs at least one comparison component",
                filename, line_num
            ));
        }

        rules.push(OptEqRule {
            line_num,
            opt_flags,
            components,
        });
    }
    Ok(rules)
}

fn extract_phase_triangulation(
    source: &str,
    filename: &str,
) -> Result<Option<PhaseTriangulation>, String> {
    let mut found: Option<PhaseTriangulation> = None;
    for (i, line) in source.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("! PHASE_TRIANGULATE:") else {
            continue;
        };
        if let Some(existing) = &found {
            return Err(format!(
                "{}:{}: multiple PHASE_TRIANGULATE annotations are not allowed (another at line {})",
                filename, line_num, existing.line_num
            ));
        }

        let mut surfaces = Vec::new();
        for token in rest.trim().split('|') {
            let surface = match token.trim() {
                "ir" => PhaseSurface::Ir,
                "asm" => PhaseSurface::Asm,
                "obj" => PhaseSurface::Obj,
                "clean" => PhaseSurface::Clean,
                "repro" => PhaseSurface::Repro,
                other => {
                    return Err(format!(
                    "{}:{}: PHASE_TRIANGULATE surfaces must be ir, asm, obj, clean, or repro; got '{}'",
                    filename, line_num, other
                ))
                }
            };
            if !surfaces.contains(&surface) {
                surfaces.push(surface);
            }
        }
        if surfaces.is_empty() {
            return Err(format!(
                "{}:{}: PHASE_TRIANGULATE needs at least one surface",
                filename, line_num
            ));
        }
        if surfaces
            .iter()
            .all(|surface| matches!(surface, PhaseSurface::Clean | PhaseSurface::Repro))
        {
            return Err(format!(
                "{}:{}: PHASE_TRIANGULATE policy-only annotations need at least one of ir, asm, or obj",
                filename, line_num
            ));
        }
        found = Some(PhaseTriangulation { line_num, surfaces });
    }
    Ok(found)
}

fn diagnostic_contains_span(stderr: &str, expected: ExpectedSpan) -> bool {
    let needle = format!("{}:{}:", expected.line, expected.col);
    stderr.contains(&needle)
}

/// A single text-shape assertion. Positive checks must appear in
/// order; negative checks must not appear at all. Source line
/// numbers are kept so failure messages can point at the right
/// annotation.
struct ShapeCheck {
    line_num: usize,
    pattern: String,
    negative: bool,
}

/// Extract positive and negative shape assertions from a source.
fn extract_shape_checks(source: &str, pos_prefix: &str, neg_prefix: &str) -> Vec<ShapeCheck> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix(pos_prefix) {
                Some(ShapeCheck {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                    negative: false,
                })
            } else if let Some(rest) = trimmed.strip_prefix(neg_prefix) {
                Some(ShapeCheck {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                    negative: true,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Extract `! IR_CHECK:` and `! IR_NOT:` annotations from a source.
fn extract_ir_checks(source: &str) -> Vec<ShapeCheck> {
    extract_shape_checks(source, "! IR_CHECK:", "! IR_NOT:")
}

/// Extract `! ASM_CHECK:` and `! ASM_NOT:` annotations from a source.
fn extract_asm_checks(source: &str) -> Vec<ShapeCheck> {
    extract_shape_checks(source, "! ASM_CHECK:", "! ASM_NOT:")
}

/// Apply IR shape assertions against an --emit-ir text dump.
/// Positive assertions match in declared order (intervening lines
/// are allowed). Negative assertions match against the entire
/// text — if the substring appears anywhere, the test fails.
fn match_shape_checks(
    checks: &[ShapeCheck],
    text: &str,
    filename: &str,
    directive_name: &str,
    full_label: &str,
) -> Result<(), String> {
    let mut search_offset = 0;
    for check in checks {
        if check.negative {
            if text.contains(&check.pattern) {
                return Err(format!(
                    "{}:{}: {} failed: substring '{}' appears in {}\n\
                     Full {}:\n{}",
                    filename,
                    check.line_num,
                    directive_name,
                    check.pattern,
                    full_label,
                    full_label,
                    text,
                ));
            }
        } else {
            // Positive: search forward from the previous match
            // position so multiple checks enforce ordering.
            if let Some(rel) = text[search_offset..].find(&check.pattern) {
                search_offset += rel + check.pattern.len();
            } else {
                return Err(format!(
                    "{}:{}: {} failed: substring '{}' not found from offset {}\n\
                     Full {}:\n{}",
                    filename,
                    check.line_num,
                    directive_name,
                    check.pattern,
                    search_offset,
                    full_label,
                    text,
                ));
            }
        }
    }
    Ok(())
}

/// Apply IR shape assertions against an --emit-ir text dump.
fn match_ir_checks(checks: &[ShapeCheck], ir: &str, filename: &str) -> Result<(), String> {
    match_shape_checks(checks, ir, filename, "IR_CHECK/IR_NOT", "IR")
}

/// Apply assembly shape assertions against a -S text dump.
fn match_asm_checks(checks: &[ShapeCheck], asm: &str, filename: &str) -> Result<(), String> {
    match_shape_checks(checks, asm, filename, "ASM_CHECK/ASM_NOT", "assembly")
}

fn match_file_checks(
    checks: &[FileCheck],
    files: &BTreeMap<String, Vec<u8>>,
    filename: &str,
) -> Result<(), String> {
    let mut search_offsets: BTreeMap<&str, usize> = BTreeMap::new();

    for check in checks {
        let Some(bytes) = files.get(&check.rel_path) else {
            return Err(format!(
                "{}:{}: FILE_CHECK/FILE_NOT expected sandbox file '{}' to exist",
                filename, check.line_num, check.rel_path
            ));
        };
        let text = String::from_utf8_lossy(bytes);
        if check.negative {
            if text.contains(&check.pattern) {
                return Err(format!(
                    "{}:{}: FILE_CHECK/FILE_NOT failed: substring '{}' appears in sandbox file '{}'\n\
                     Full file contents:\n{}",
                    filename, check.line_num, check.pattern, check.rel_path, text
                ));
            }
        } else {
            let search_offset = search_offsets.entry(&check.rel_path).or_insert(0);
            if let Some(rel) = text[*search_offset..].find(&check.pattern) {
                *search_offset += rel + check.pattern.len();
            } else {
                return Err(format!(
                    "{}:{}: FILE_CHECK/FILE_NOT failed: substring '{}' not found in sandbox file '{}' from offset {}\n\
                     Full file contents:\n{}",
                    filename,
                    check.line_num,
                    check.pattern,
                    check.rel_path,
                    *search_offset,
                    text
                ));
            }
        }
    }

    Ok(())
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

fn match_file_presence_checks(
    checks: &[FilePresenceCheck],
    files: &BTreeMap<String, Vec<u8>>,
    filename: &str,
) -> Result<(), String> {
    for check in checks {
        let exists = files.contains_key(&check.rel_path);
        if check.should_exist && !exists {
            return Err(format!(
                "{}:{}: FILE_EXISTS failed: sandbox file '{}' was not created",
                filename, check.line_num, check.rel_path
            ));
        }
        if !check.should_exist && exists {
            return Err(format!(
                "{}:{}: FILE_MISSING failed: sandbox file '{}' was created",
                filename, check.line_num, check.rel_path
            ));
        }
    }
    Ok(())
}

fn match_file_line_count_checks(
    checks: &[FileLineCountCheck],
    files: &BTreeMap<String, Vec<u8>>,
    filename: &str,
) -> Result<(), String> {
    for check in checks {
        let Some(bytes) = files.get(&check.rel_path) else {
            return Err(format!(
                "{}:{}: FILE_LINE_COUNT expected sandbox file '{}' to exist",
                filename, check.line_num, check.rel_path
            ));
        };
        let text = String::from_utf8_lossy(bytes);
        let actual_lines = text.lines().count();
        if actual_lines != check.expected_lines {
            return Err(format!(
                "{}:{}: FILE_LINE_COUNT failed for '{}': expected {} lines, got {}\n\
                 Full file contents:\n{}",
                filename, check.line_num, check.rel_path, check.expected_lines, actual_lines, text
            ));
        }
    }
    Ok(())
}

fn match_file_rerun_mode_checks(
    checks: &[FileRerunModeCheck],
    first: &RunSnapshot,
    second: &RunSnapshot,
    filename: &str,
) -> Result<(), String> {
    for check in checks {
        let Some(first_bytes) = first.files.get(&check.rel_path) else {
            return Err(format!(
                "{}:{}: FILE_RERUN_MODE expected sandbox file '{}' after first run",
                filename, check.line_num, check.rel_path
            ));
        };
        let Some(second_bytes) = second.files.get(&check.rel_path) else {
            return Err(format!(
                "{}:{}: FILE_RERUN_MODE expected sandbox file '{}' after second run",
                filename, check.line_num, check.rel_path
            ));
        };

        match check.mode {
            FileRerunMode::Stable if first_bytes != second_bytes => {
                return Err(format!(
                    "{}:{}: FILE_RERUN_MODE(stable) failed for '{}': file contents changed across rerun",
                    filename, check.line_num, check.rel_path
                ));
            }
            FileRerunMode::Append => {
                if second_bytes.len() <= first_bytes.len() || !second_bytes.starts_with(first_bytes)
                {
                    return Err(format!(
                        "{}:{}: FILE_RERUN_MODE(append) failed for '{}': second run did not strictly append to first-run contents",
                        filename, check.line_num, check.rel_path
                    ));
                }
            }
            FileRerunMode::Stable => {}
        }
    }
    Ok(())
}

fn match_file_set_exact(
    check: &FileSetExactCheck,
    files: &BTreeMap<String, Vec<u8>>,
    filename: &str,
) -> Result<(), String> {
    let actual = files.keys().cloned().collect::<Vec<_>>();
    if actual != check.rel_paths {
        return Err(format!(
            "{}:{}: FILE_SET_EXACT failed: expected {:?}, got {:?}",
            filename, check.line_num, check.rel_paths, actual
        ));
    }
    Ok(())
}

fn collect_declared_runtime_paths(
    file_checks: &[FileCheck],
    file_presence_checks: &[FilePresenceCheck],
    file_line_count_checks: &[FileLineCountCheck],
    file_rerun_mode_checks: &[FileRerunModeCheck],
    file_set_exact: Option<&FileSetExactCheck>,
) -> BTreeMap<String, String> {
    let mut paths = BTreeMap::new();
    for check in file_checks {
        paths.entry(check.rel_path.clone()).or_insert_with(|| {
            if check.negative {
                "FILE_NOT".to_string()
            } else {
                "FILE_CHECK".to_string()
            }
        });
    }
    for check in file_presence_checks {
        paths.entry(check.rel_path.clone()).or_insert_with(|| {
            if check.should_exist {
                "FILE_EXISTS".to_string()
            } else {
                "FILE_MISSING".to_string()
            }
        });
    }
    for check in file_line_count_checks {
        paths
            .entry(check.rel_path.clone())
            .or_insert_with(|| "FILE_LINE_COUNT".to_string());
    }
    for check in file_rerun_mode_checks {
        paths
            .entry(check.rel_path.clone())
            .or_insert_with(|| "FILE_RERUN_MODE".to_string());
    }
    if let Some(check) = file_set_exact {
        for rel_path in &check.rel_paths {
            paths
                .entry(rel_path.clone())
                .or_insert_with(|| "FILE_SET_EXACT".to_string());
        }
    }
    paths
}

fn snapshot_sandbox_files(
    sandbox: &Path,
    filename: &str,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    collect_sandbox_files(sandbox, sandbox, &mut files).map_err(|e| {
        format!(
            "{}: cannot snapshot sandbox {}: {}",
            filename,
            sandbox.display(),
            e
        )
    })?;
    Ok(files)
}

#[derive(Debug)]
struct PhaseArtifact {
    sandbox_files: BTreeMap<String, Vec<u8>>,
    output_rel_path: String,
    output_bytes: Vec<u8>,
}

fn run_binary_in_sandbox(
    binary: &Path,
    sandbox: &Path,
    filename: &str,
) -> Result<RunSnapshot, String> {
    let run = Command::new(binary)
        .current_dir(sandbox)
        .output()
        .map_err(|e| format!("{}: cannot run binary: {}", filename, e))?;
    Ok(RunSnapshot {
        exit_code: run.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&run.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&run.stderr).into_owned(),
        files: snapshot_sandbox_files(sandbox, filename)?,
    })
}

fn compile_phase_artifact(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    phase: PhaseSurface,
    filename: &str,
) -> Result<PhaseArtifact, String> {
    let compiler_path = fs::canonicalize(compiler)
        .map_err(|e| format!("{}: cannot canonicalize compiler path: {}", filename, e))?;
    let source_path = fs::canonicalize(source)
        .map_err(|e| format!("{}: cannot canonicalize source path: {}", filename, e))?;
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let phase_name = match phase {
        PhaseSurface::Ir => "ir",
        PhaseSurface::Asm => "asm",
        PhaseSurface::Obj => "obj",
        PhaseSurface::Clean => unreachable!("clean is a triangulation policy, not an artifact"),
        PhaseSurface::Repro => unreachable!("repro is a triangulation policy, not an artifact"),
    };
    let phase_sandbox = unique_temp_path(
        "phase_sandbox",
        stem,
        &format!("{}_{}", level, phase_name),
        "",
    );
    fs::create_dir_all(&phase_sandbox).map_err(|e| {
        format!(
            "{}: cannot create phase sandbox dir {}: {}",
            filename,
            phase_sandbox.display(),
            e
        )
    })?;
    let (output_name, extra_args): (&str, &[&str]) = match phase {
        PhaseSurface::Ir => ("phase-output.ir", &["--emit-ir"]),
        PhaseSurface::Asm => ("phase-output.s", &["-S"]),
        PhaseSurface::Obj => ("phase-output.o", &["-c"]),
        PhaseSurface::Clean => unreachable!(),
        PhaseSurface::Repro => unreachable!(),
    };
    let output_path = phase_sandbox.join(output_name);

    let compile = Command::new(&compiler_path)
        .current_dir(&phase_sandbox)
        .args([source_path.to_str().unwrap(), opt_flag])
        .args(extra_args)
        .args(["-o", output_path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("{}: cannot run {} compile: {}", filename, phase_name, e))?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        let _ = fs::remove_dir_all(&phase_sandbox);
        return Err(format!(
            "{} [{}]: {} compilation failed:\n{}",
            filename, opt_flag, phase_name, stderr
        ));
    }

    let output_bytes = fs::read(&output_path).map_err(|e| {
        format!(
            "{}: cannot read {} output {}: {}",
            filename,
            phase_name,
            output_path.display(),
            e
        )
    })?;
    let sandbox_files = snapshot_sandbox_files(&phase_sandbox, filename)?;
    let artifact = PhaseArtifact {
        sandbox_files,
        output_rel_path: output_name.to_string(),
        output_bytes,
    };
    let _ = fs::remove_dir_all(&phase_sandbox);
    Ok(artifact)
}

/// Match checks against actual output lines. Checks must appear in order
/// but not necessarily consecutively — intervening output lines are allowed.
fn match_checks(
    checks: &[Check],
    output: &str,
    filename: &str,
    directive_name: &str,
) -> Result<(), String> {
    let output_lines: Vec<&str> = output.lines().collect();
    let mut output_idx = 0;

    for check in checks {
        let mut found = false;
        while output_idx < output_lines.len() {
            if output_lines[output_idx].trim().contains(&check.pattern) {
                found = true;
                output_idx += 1;
                break;
            }
            output_idx += 1;
        }
        if !found {
            return Err(format!(
                "{}:{}: {} failed: expected '{}' not found in remaining output\n\
                 Full output:\n{}",
                filename, check.line_num, directive_name, check.pattern, output
            ));
        }
    }

    Ok(())
}

/// Find the armfortas binary.
fn find_compiler() -> PathBuf {
    for dir in candidate_target_dirs() {
        let p = dir.join("armfortas");
        if p.exists() {
            return fs::canonicalize(&p).unwrap_or_else(|e| {
                panic!("cannot canonicalize compiler path {}: {}", p.display(), e)
            });
        }
    }
    panic!("cannot find armfortas binary — run `cargo build` first");
}

/// Find the test_programs directory.
fn find_test_programs() -> PathBuf {
    let candidates = ["test_programs", "../test_programs"];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return p;
        }
    }
    panic!("cannot find test_programs/ directory");
}

fn is_test_program_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("f90" | "f95" | "f03" | "f08" | "f18" | "f23" | "f" | "for" | "ftn" | "fpp")
    )
}

fn compile_stage_bytes(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    stage: ReproStage,
) -> Result<Vec<u8>, String> {
    let source_path = fs::canonicalize(source).map_err(|e| {
        format!(
            "{}: cannot canonicalize source path: {}",
            source.display(),
            e
        )
    })?;
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let (kind, ext, extra_args): (&str, &str, &[&str]) = match stage {
        ReproStage::Asm => ("asm", ".s", &["-S"]),
        ReproStage::Obj => ("obj", ".o", &["-c"]),
        ReproStage::Run => unreachable!("run reproducibility uses runtime snapshots"),
        ReproStage::RunSameSandbox => {
            unreachable!("same-sandbox reproducibility uses runtime snapshots")
        }
    };
    let out = unique_temp_path(kind, stem, level, ext);
    let compile_sandbox =
        unique_temp_path("compile_sandbox", stem, &format!("{}_{}", level, kind), "");
    fs::create_dir_all(&compile_sandbox).map_err(|e| {
        format!(
            "{}: cannot create {} compile sandbox {}: {}",
            source.display(),
            kind,
            compile_sandbox.display(),
            e
        )
    })?;
    let compile = Command::new(compiler)
        .current_dir(&compile_sandbox)
        .args([source_path.to_str().unwrap(), opt_flag])
        .args(extra_args)
        .args(["-o", out.to_str().unwrap()])
        .output()
        .map_err(|e| {
            format!(
                "{}: cannot run compiler for {} repro: {}",
                source.display(),
                kind,
                e
            )
        })?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        let _ = fs::remove_file(&out);
        let _ = fs::remove_dir_all(&compile_sandbox);
        return Err(format!(
            "{} [{}]: {} reproducibility compile failed:\n{}",
            source.file_name().unwrap().to_string_lossy(),
            opt_flag,
            kind,
            stderr
        ));
    }
    let bytes = fs::read(&out).map_err(|e| {
        format!(
            "{}: cannot read {} output {}: {}",
            source.display(),
            kind,
            out.display(),
            e
        )
    })?;
    let _ = fs::remove_file(&out);
    let _ = fs::remove_dir_all(&compile_sandbox);
    Ok(bytes)
}

fn compile_and_run_snapshot(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    filename: &str,
) -> Result<RunSnapshot, String> {
    let source_path = fs::canonicalize(source)
        .map_err(|e| format!("{}: cannot canonicalize source path: {}", filename, e))?;
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let binary = unique_temp_path("test_bin", stem, level, "");
    let sandbox = unique_temp_path("test_sandbox", stem, &format!("{}_opt_eq", level), "");
    let compile_sandbox =
        unique_temp_path("compile_sandbox", stem, &format!("{}_opt_eq", level), "");
    fs::create_dir_all(&compile_sandbox).map_err(|e| {
        format!(
            "{}: cannot create OPT_EQ compile sandbox {}: {}",
            filename,
            compile_sandbox.display(),
            e
        )
    })?;

    let compile = Command::new(compiler)
        .current_dir(&compile_sandbox)
        .args([
            source_path.to_str().unwrap(),
            opt_flag,
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("{}: cannot run compiler: {}", filename, e))?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        let _ = fs::remove_dir_all(&compile_sandbox);
        let _ = fs::remove_file(&binary);
        return Err(format!(
            "{} [{}]: OPT_EQ comparison compile failed:\n{}",
            filename, opt_flag, stderr
        ));
    }

    fs::create_dir_all(&sandbox).map_err(|e| {
        format!(
            "{}: cannot create OPT_EQ sandbox dir {}: {}",
            filename,
            sandbox.display(),
            e
        )
    })?;
    let snapshot = run_binary_in_sandbox(&binary, &sandbox, filename)?;
    let _ = fs::remove_file(&binary);
    let _ = fs::remove_dir_all(&compile_sandbox);
    let _ = fs::remove_dir_all(&sandbox);
    Ok(snapshot)
}

fn render_opt_eq_components(components: &[OptEqComponent]) -> String {
    components
        .iter()
        .map(|component| match component {
            OptEqComponent::Stdout => "stdout",
            OptEqComponent::Stderr => "stderr",
            OptEqComponent::Exit => "exit",
            OptEqComponent::Asm => "asm",
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn compare_opt_eq_runtime_components(
    baseline: &RunSnapshot,
    other: &RunSnapshot,
    components: &[OptEqComponent],
) -> Option<String> {
    for component in components {
        match component {
            OptEqComponent::Exit if baseline.exit_code != other.exit_code => {
                return Some(format!(
                    "exit mismatch: baseline {}, other {}",
                    baseline.exit_code, other.exit_code
                ))
            }
            OptEqComponent::Stdout if baseline.stdout != other.stdout => {
                return Some(format!(
                    "stdout mismatch:\nbaseline:\n{}\nother:\n{}",
                    baseline.stdout, other.stdout
                ))
            }
            OptEqComponent::Stderr if baseline.stderr != other.stderr => {
                return Some(format!(
                    "stderr mismatch:\nbaseline:\n{}\nother:\n{}",
                    baseline.stderr, other.stderr
                ))
            }
            _ => {}
        }
    }
    None
}

fn run_opt_eq_rules(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    filename: &str,
    baseline_snapshot: &RunSnapshot,
    rules: &[OptEqRule],
) -> Result<(), String> {
    for rule in rules {
        if rule.opt_flags.first().map(String::as_str) != Some(opt_flag) {
            continue;
        }

        let baseline_asm = if rule.components.contains(&OptEqComponent::Asm) {
            Some(compile_stage_bytes(
                compiler,
                source,
                opt_flag,
                ReproStage::Asm,
            )?)
        } else {
            None
        };

        for compare_opt in rule.opt_flags.iter().skip(1) {
            if rule
                .components
                .iter()
                .any(|component| *component != OptEqComponent::Asm)
            {
                let other = compile_and_run_snapshot(compiler, source, compare_opt, filename)?;
                if let Some(detail) =
                    compare_opt_eq_runtime_components(baseline_snapshot, &other, &rule.components)
                {
                    return Err(format!(
                        "{} [{}]: OPT_EQ({} => {}) failed comparing {} to {}: {}",
                        filename,
                        opt_flag,
                        rule.opt_flags
                            .iter()
                            .map(|flag| flag.trim_start_matches('-'))
                            .collect::<Vec<_>>()
                            .join(","),
                        render_opt_eq_components(&rule.components),
                        opt_flag,
                        compare_opt,
                        detail,
                    ));
                }
            }

            if let Some(baseline_asm) = &baseline_asm {
                let other_asm =
                    compile_stage_bytes(compiler, source, compare_opt, ReproStage::Asm)?;
                if baseline_asm != &other_asm {
                    return Err(format!(
                        "{} [{}]: OPT_EQ({} => {}) failed comparing {} to {}: assembly output differed",
                        filename,
                        opt_flag,
                        rule
                            .opt_flags
                            .iter()
                            .map(|flag| flag.trim_start_matches('-'))
                            .collect::<Vec<_>>()
                            .join(","),
                        render_opt_eq_components(&rule.components),
                        opt_flag,
                        compare_opt,
                    ));
                }
            }
        }
    }

    Ok(())
}

fn render_phase_surfaces(surfaces: &[PhaseSurface]) -> String {
    surfaces
        .iter()
        .map(|surface| match surface {
            PhaseSurface::Ir => "ir",
            PhaseSurface::Asm => "asm",
            PhaseSurface::Obj => "obj",
            PhaseSurface::Clean => "clean",
            PhaseSurface::Repro => "repro",
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn run_phase_triangulation(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    filename: &str,
    triangulation: &PhaseTriangulation,
    declared_runtime_paths: &BTreeMap<String, String>,
) -> Result<(), String> {
    let require_clean = triangulation.surfaces.contains(&PhaseSurface::Clean);
    let require_repro = triangulation.surfaces.contains(&PhaseSurface::Repro);
    for surface in &triangulation.surfaces {
        match surface {
            PhaseSurface::Ir | PhaseSurface::Asm | PhaseSurface::Obj => {
                let artifact =
                    compile_phase_artifact(compiler, source, opt_flag, *surface, filename)?;
                if artifact.output_bytes.is_empty() {
                    let surface_name = match surface {
                        PhaseSurface::Ir => "IR",
                        PhaseSurface::Asm => "assembly",
                        PhaseSurface::Obj => "object",
                        PhaseSurface::Clean => unreachable!(),
                        PhaseSurface::Repro => unreachable!(),
                    };
                    return Err(format!(
                        "{}:{}: PHASE_TRIANGULATE({}) produced empty {} output at {}",
                        filename,
                        triangulation.line_num,
                        render_phase_surfaces(&triangulation.surfaces),
                        surface_name,
                        opt_flag,
                    ));
                }

                if require_clean {
                    let file_keys: Vec<&str> = artifact
                        .sandbox_files
                        .keys()
                        .map(|key| key.as_str())
                        .collect();
                    if file_keys != vec![artifact.output_rel_path.as_str()] {
                        let runtime_leaks = declared_runtime_paths
                            .iter()
                            .filter(|(path, _)| artifact.sandbox_files.contains_key(path.as_str()))
                            .map(|(path, directive)| format!("{} ({})", path, directive))
                            .collect::<Vec<_>>();
                        if !runtime_leaks.is_empty() {
                            return Err(format!(
                                "{}:{}: PHASE_TRIANGULATE({}) failed at {}: compile-only phase created runtime side-effect files: {}",
                                filename,
                                triangulation.line_num,
                                render_phase_surfaces(&triangulation.surfaces),
                                opt_flag,
                                runtime_leaks.join(", "),
                            ));
                        }
                        return Err(format!(
                            "{}:{}: PHASE_TRIANGULATE({}) failed at {}: compile-only phase left unexpected files {:?} (expected only '{}')",
                            filename,
                            triangulation.line_num,
                            render_phase_surfaces(&triangulation.surfaces),
                            opt_flag,
                            file_keys,
                            artifact.output_rel_path,
                        ));
                    }
                }
                if require_repro {
                    let second_artifact =
                        compile_phase_artifact(compiler, source, opt_flag, *surface, filename)?;
                    if require_clean {
                        let second_file_keys: Vec<&str> = second_artifact
                            .sandbox_files
                            .keys()
                            .map(|key| key.as_str())
                            .collect();
                        if second_file_keys != vec![second_artifact.output_rel_path.as_str()] {
                            return Err(format!(
                                "{}:{}: PHASE_TRIANGULATE({}) failed at {}: repeated compile-only phase left unexpected files {:?} (expected only '{}')",
                                filename,
                                triangulation.line_num,
                                render_phase_surfaces(&triangulation.surfaces),
                                opt_flag,
                                second_file_keys,
                                second_artifact.output_rel_path,
                            ));
                        }
                    }
                    if artifact.output_bytes != second_artifact.output_bytes {
                        let surface_name = match surface {
                            PhaseSurface::Ir => "IR",
                            PhaseSurface::Asm => "assembly",
                            PhaseSurface::Obj => "object",
                            PhaseSurface::Clean => unreachable!(),
                            PhaseSurface::Repro => unreachable!(),
                        };
                        return Err(format!(
                            "{}:{}: PHASE_TRIANGULATE({}) failed at {}: {} output changed across repeated compile-only runs",
                            filename,
                            triangulation.line_num,
                            render_phase_surfaces(&triangulation.surfaces),
                            opt_flag,
                            surface_name,
                        ));
                    }
                }
            }
            PhaseSurface::Clean | PhaseSurface::Repro => {}
        }
    }
    Ok(())
}

fn describe_run_difference(first: &RunSnapshot, second: &RunSnapshot) -> String {
    if first.exit_code != second.exit_code {
        return format!(
            "exit code mismatch: first {}, second {}",
            first.exit_code, second.exit_code
        );
    }
    if first.stdout != second.stdout {
        return format!(
            "stdout mismatch:\nfirst:\n{}\nsecond:\n{}",
            first.stdout, second.stdout
        );
    }
    if first.stderr != second.stderr {
        return format!(
            "stderr mismatch:\nfirst:\n{}\nsecond:\n{}",
            first.stderr, second.stderr
        );
    }
    if first.files.keys().collect::<Vec<_>>() != second.files.keys().collect::<Vec<_>>() {
        return format!(
            "sandbox file set mismatch: first {:?}, second {:?}",
            first.files.keys().collect::<Vec<_>>(),
            second.files.keys().collect::<Vec<_>>()
        );
    }
    for (path, first_bytes) in &first.files {
        let second_bytes = &second.files[path];
        if first_bytes != second_bytes {
            return format!("sandbox file contents differ for '{}'", path);
        }
    }
    "unknown runtime observation mismatch".to_string()
}

/// What happened when we ran a test program.
#[derive(Debug)]
enum TestOutcome {
    /// Compiled, ran, all CHECKs matched. No XFAIL annotation present.
    Pass,
    /// Marked XFAIL and failed somewhere — this is the expected
    /// outcome for an open audit finding. The reason is the XFAIL
    /// annotation text plus the underlying failure detail.
    Xfail(String),
    /// Marked XFAIL but unexpectedly succeeded. Loud failure: the
    /// underlying bug is fixed and the XFAIL annotation should be
    /// removed so the program becomes a regular regression test.
    Xpass(String),
    /// No XFAIL annotation, and the program failed somewhere.
    Fail(String),
}

/// Run a single test program: compile at the given optimization level,
/// execute, check output. Honors `! XFAIL:` annotations.
fn run_test(compiler: &Path, source: &Path, opt_flag: &str) -> TestOutcome {
    let filename = source.file_name().unwrap().to_str().unwrap();
    let source_text = match fs::read_to_string(source) {
        Ok(s) => s,
        Err(e) => return TestOutcome::Fail(format!("{}: cannot read: {}", filename, e)),
    };

    let xfail_reason = extract_xfail(&source_text);
    let error_expected = extract_error_expected(&source_text);
    let error_span = match extract_error_span(&source_text, filename) {
        Ok(span) => span,
        Err(e) => return TestOutcome::Fail(e),
    };
    let checks = extract_checks(&source_text);
    let stderr_checks = extract_stderr_checks(&source_text);
    let expected_exit_code = match extract_exit_code(&source_text, filename) {
        Ok(code) => code,
        Err(e) => return TestOutcome::Fail(e),
    };
    let ir_checks = extract_ir_checks(&source_text);
    let asm_checks = extract_asm_checks(&source_text);
    let file_checks =
        match extract_file_checks(&source_text, filename, "! FILE_CHECK:", "! FILE_NOT:") {
            Ok(checks) => checks,
            Err(e) => return TestOutcome::Fail(e),
        };
    let file_presence_checks = match extract_file_presence_checks(&source_text, filename) {
        Ok(checks) => checks,
        Err(e) => return TestOutcome::Fail(e),
    };
    let file_line_count_checks = match extract_file_line_count_checks(&source_text, filename) {
        Ok(checks) => checks,
        Err(e) => return TestOutcome::Fail(e),
    };
    let file_rerun_mode_checks = match extract_file_rerun_mode_checks(&source_text, filename) {
        Ok(checks) => checks,
        Err(e) => return TestOutcome::Fail(e),
    };
    let file_set_exact = match extract_file_set_exact(&source_text, filename) {
        Ok(check) => check,
        Err(e) => return TestOutcome::Fail(e),
    };
    let repro_checks = match extract_repro_checks(&source_text, filename) {
        Ok(checks) => checks,
        Err(e) => return TestOutcome::Fail(e),
    };
    let opt_eq_rules = match extract_opt_eq_rules(&source_text, filename) {
        Ok(rules) => rules,
        Err(e) => return TestOutcome::Fail(e),
    };
    let phase_triangulation = match extract_phase_triangulation(&source_text, filename) {
        Ok(rule) => rule,
        Err(e) => return TestOutcome::Fail(e),
    };
    if checks.is_empty()
        && stderr_checks.is_empty()
        && ir_checks.is_empty()
        && asm_checks.is_empty()
        && file_checks.is_empty()
        && file_presence_checks.is_empty()
        && file_line_count_checks.is_empty()
        && file_rerun_mode_checks.is_empty()
        && file_set_exact.is_none()
        && repro_checks.is_empty()
        && opt_eq_rules.is_empty()
        && phase_triangulation.is_none()
        && expected_exit_code.is_none()
        && error_span.is_none()
        && xfail_reason.is_none()
        && error_expected.is_none()
    {
        // Programs with no runtime or shape assertions, no XFAIL marker,
        // and no ERROR marker are mis-configured tests, not test failures.
        return TestOutcome::Fail(format!(
            "{}: no CHECK / STDERR_CHECK / EXIT_CODE / IR_CHECK / ASM_CHECK / FILE_CHECK / FILE_EXISTS / FILE_MISSING / FILE_LINE_COUNT / FILE_RERUN_MODE / FILE_SET_EXACT / REPRO_CHECK / XFAIL / ERROR_EXPECTED / ERROR_SPAN annotations",
            filename,
        ));
    }
    if error_span.is_some() && error_expected.is_none() {
        return TestOutcome::Fail(format!(
            "{}: ERROR_SPAN requires ERROR_EXPECTED so the harness knows which compile failure to validate",
            filename,
        ));
    }
    if phase_triangulation.is_some() && error_expected.is_some() {
        return TestOutcome::Fail(format!(
            "{}: PHASE_TRIANGULATE is for successful compile/run tests and cannot be combined with ERROR_EXPECTED",
            filename,
        ));
    }
    if !file_rerun_mode_checks.is_empty() && repro_checks.contains(&ReproStage::RunSameSandbox) {
        return TestOutcome::Fail(format!(
            "{}: FILE_RERUN_MODE is a same-sandbox rerun oracle and cannot be combined with REPRO_CHECK(run_same_sandbox)",
            filename,
        ));
    }

    // Try the compile/run/check pipeline. Any failure path returns
    // an Err with a message; success returns Ok.
    let multifile_segments = split_multifile_segments(&source_text);
    let multifile_link_order = extract_multifile_link(&source_text);

    let inner = || -> Result<(), String> {
        // Use a per-(file,level) binary path so concurrent jobs
        // and successive runs at different levels don't stomp each other.
        let stem = source.file_stem().unwrap().to_str().unwrap();
        let level = opt_flag.trim_start_matches('-');
        let binary = unique_temp_path("test_bin", stem, level, "");

        // ---- Multi-file path: split, compile each to .o, link ----
        if let Some(segments) = &multifile_segments {
            let build_dir = unique_temp_path("multifile_build", stem, level, "");
            fs::create_dir_all(&build_dir)
                .map_err(|e| format!("{}: cannot create multifile build dir: {}", filename, e))?;

            // Write each segment to its own .f90.
            for seg in segments {
                let seg_path = build_dir.join(&seg.name);
                fs::write(&seg_path, &seg.source).map_err(|e| {
                    format!("{}: cannot write segment {}: {}", filename, seg.name, e)
                })?;
            }

            // Determine compilation order from MULTIFILE_LINK or declaration order.
            let ordered_names: Vec<&str> = if let Some(link_order) = &multifile_link_order {
                link_order.iter().map(|s| s.as_str()).collect()
            } else {
                segments.iter().map(|s| s.name.as_str()).collect()
            };

            // Compile each in order (dependencies first).
            let mut objects = Vec::new();
            let mut compile_error: Option<String> = None;
            for name in &ordered_names {
                let seg_f90 = build_dir.join(name);
                if !seg_f90.exists() {
                    let msg = format!(
                        "{}: MULTIFILE_LINK references '{}' but no !--- file: segment defines it",
                        filename, name,
                    );
                    compile_error = Some(msg);
                    break;
                }
                let seg_o = build_dir.join(format!(
                    "{}.o",
                    Path::new(name).file_stem().unwrap().to_str().unwrap()
                ));
                if let Err(e) = compile_to_object(compiler, &seg_f90, &seg_o, opt_flag, &build_dir)
                {
                    compile_error = Some(format!("{} [{}]: {}", filename, opt_flag, e));
                    break;
                }
                objects.push(seg_o);
            }

            // Handle ERROR_EXPECTED for multi-file tests.
            if let Some(err_msg) = compile_error {
                let _ = fs::remove_dir_all(&build_dir);
                if let Some(expected) = &error_expected {
                    if err_msg.contains(expected.as_str()) {
                        return Ok(());
                    }
                    return Err(format!(
                        "{} [{}]: ERROR_EXPECTED({}) but compile error did not contain it.\n\
                         Actual error:\n{}",
                        filename, opt_flag, expected, err_msg,
                    ));
                }
                return Err(err_msg);
            }

            // ERROR_EXPECTED but compilation succeeded — that's a failure.
            if error_expected.is_some() {
                let _ = fs::remove_dir_all(&build_dir);
                return Err(format!(
                    "{} [{}]: ERROR_EXPECTED but all segments compiled successfully",
                    filename, opt_flag,
                ));
            }

            // Link all .o files into a binary.
            link_objects(&objects, &binary)
                .map_err(|e| format!("{} [{}]: {}", filename, opt_flag, e))?;

            let _ = fs::remove_dir_all(&build_dir);
        } else {
            // ---- Single-file path ----
            let source_path = fs::canonicalize(source).map_err(|e| {
                format!(
                    "{}: cannot canonicalize source {}: {}",
                    filename,
                    source.display(),
                    e
                )
            })?;
            let compile_sandbox = unique_temp_path("compile_sandbox", stem, level, "");
            fs::create_dir_all(&compile_sandbox).map_err(|e| {
                format!(
                    "{}: cannot create compile sandbox dir {}: {}",
                    filename,
                    compile_sandbox.display(),
                    e
                )
            })?;
            let compile = Command::new(compiler)
                .current_dir(&compile_sandbox)
                .args([
                    source_path.to_str().unwrap(),
                    opt_flag,
                    "-o",
                    binary.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| format!("{}: cannot run compiler: {}", filename, e))?;

            // ERROR_EXPECTED branch: compilation MUST fail with the
            // expected stderr substring. CHECKs are ignored.
            if let Some(expected) = &error_expected {
                if compile.status.success() {
                    let _ = fs::remove_dir_all(&compile_sandbox);
                    let _ = fs::remove_file(&binary);
                    return Err(format!(
                        "{} [{}]: ERROR_EXPECTED({}) but compilation succeeded",
                        filename, opt_flag, expected,
                    ));
                }
                let stderr = String::from_utf8_lossy(&compile.stderr);
                if !stderr.contains(expected.as_str()) {
                    let _ = fs::remove_dir_all(&compile_sandbox);
                    return Err(format!(
                        "{} [{}]: ERROR_EXPECTED({}) but stderr did not contain it.\n\
                         Full stderr:\n{}",
                        filename, opt_flag, expected, stderr,
                    ));
                }
                if let Some(expected_span) = error_span {
                    if !diagnostic_contains_span(&stderr, expected_span) {
                        let _ = fs::remove_dir_all(&compile_sandbox);
                        return Err(format!(
                            "{} [{}]: ERROR_SPAN({}:{}) but stderr did not contain that location.\n\
                             Full stderr:\n{}",
                            filename, opt_flag, expected_span.line, expected_span.col, stderr,
                        ));
                    }
                }
                let _ = fs::remove_dir_all(&compile_sandbox);
                return Ok(());
            }

            if !compile.status.success() {
                let stderr = String::from_utf8_lossy(&compile.stderr);
                let _ = fs::remove_dir_all(&compile_sandbox);
                return Err(format!(
                    "{} [{}]: compilation failed:\n{}",
                    filename, opt_flag, stderr,
                ));
            }
            let _ = fs::remove_dir_all(&compile_sandbox);
        }

        // Per-(file,level) sandbox directory. Test programs that touch the
        // filesystem (open(file=...)) write into this directory via relative
        // paths, which keeps the parallel test_programs_end_to_end_o*
        // threads from racing on shared paths.
        let sandbox = unique_temp_path("test_sandbox", stem, level, "");
        fs::create_dir_all(&sandbox).map_err(|e| {
            format!(
                "{}: cannot create sandbox dir {}: {}",
                filename,
                sandbox.display(),
                e
            )
        })?;

        let snapshot = run_binary_in_sandbox(&binary, &sandbox, filename)?;

        let actual_exit_code = snapshot.exit_code;
        let stderr = &snapshot.stderr;
        let expected_exit_code = expected_exit_code.unwrap_or(0);
        if actual_exit_code != expected_exit_code {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(format!(
                "{} [{}]: execution exit mismatch: expected {}, got {}\n\
                 stderr:\n{}",
                filename, opt_flag, expected_exit_code, actual_exit_code, stderr,
            ));
        }

        let stdout = &snapshot.stdout;
        let label = format!("{} [{}]", filename, opt_flag);
        if let Err(e) = match_checks(&checks, &stdout, &label, "CHECK") {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Err(e) = match_checks(&stderr_checks, &stderr, &label, "STDERR_CHECK") {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Err(e) = match_file_checks(&file_checks, &snapshot.files, &label) {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Err(e) = match_file_presence_checks(&file_presence_checks, &snapshot.files, &label) {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Err(e) =
            match_file_line_count_checks(&file_line_count_checks, &snapshot.files, &label)
        {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Some(check) = &file_set_exact {
            if let Err(e) = match_file_set_exact(check, &snapshot.files, &label) {
                let _ = fs::remove_file(&binary);
                let _ = fs::remove_dir_all(&sandbox);
                return Err(e);
            }
        }
        if !file_rerun_mode_checks.is_empty() {
            let second = run_binary_in_sandbox(&binary, &sandbox, filename)?;
            if snapshot.exit_code != second.exit_code {
                let _ = fs::remove_file(&binary);
                let _ = fs::remove_dir_all(&sandbox);
                return Err(format!(
                    "{} [{}]: FILE_RERUN_MODE rerun exit mismatch: first {}, second {}",
                    filename, opt_flag, snapshot.exit_code, second.exit_code
                ));
            }
            if snapshot.stdout != second.stdout {
                let _ = fs::remove_file(&binary);
                let _ = fs::remove_dir_all(&sandbox);
                return Err(format!(
                    "{} [{}]: FILE_RERUN_MODE rerun stdout mismatch:\nfirst:\n{}\nsecond:\n{}",
                    filename, opt_flag, snapshot.stdout, second.stdout
                ));
            }
            if snapshot.stderr != second.stderr {
                let _ = fs::remove_file(&binary);
                let _ = fs::remove_dir_all(&sandbox);
                return Err(format!(
                    "{} [{}]: FILE_RERUN_MODE rerun stderr mismatch:\nfirst:\n{}\nsecond:\n{}",
                    filename, opt_flag, snapshot.stderr, second.stderr
                ));
            }
            if let Err(e) =
                match_file_rerun_mode_checks(&file_rerun_mode_checks, &snapshot, &second, &label)
            {
                let _ = fs::remove_file(&binary);
                let _ = fs::remove_dir_all(&sandbox);
                return Err(e);
            }
        }
        for stage in &repro_checks {
            match stage {
                ReproStage::Asm | ReproStage::Obj => {
                    let first = compile_stage_bytes(compiler, source, opt_flag, *stage)?;
                    let second = compile_stage_bytes(compiler, source, opt_flag, *stage)?;
                    if first != second {
                        let stage_name = match stage {
                            ReproStage::Asm => "asm",
                            ReproStage::Obj => "obj",
                            ReproStage::Run => unreachable!(),
                            ReproStage::RunSameSandbox => unreachable!(),
                        };
                        let _ = fs::remove_file(&binary);
                        let _ = fs::remove_dir_all(&sandbox);
                        return Err(format!(
                            "{} [{}]: REPRO_CHECK({}) failed: two compilations produced different {} bytes",
                            filename, opt_flag, stage_name, stage_name
                        ));
                    }
                }
                ReproStage::Run => {
                    let repro_sandbox =
                        unique_temp_path("test_sandbox", stem, &format!("{}_repro", level), "");
                    fs::create_dir_all(&repro_sandbox).map_err(|e| {
                        format!(
                            "{}: cannot create repro sandbox dir {}: {}",
                            filename,
                            repro_sandbox.display(),
                            e
                        )
                    })?;
                    let second = run_binary_in_sandbox(&binary, &repro_sandbox, filename)?;
                    let _ = fs::remove_dir_all(&repro_sandbox);
                    if snapshot != second {
                        let detail = describe_run_difference(&snapshot, &second);
                        let _ = fs::remove_file(&binary);
                        let _ = fs::remove_dir_all(&sandbox);
                        return Err(format!(
                            "{} [{}]: REPRO_CHECK(run) failed: {}",
                            filename, opt_flag, detail
                        ));
                    }
                }
                ReproStage::RunSameSandbox => {
                    let second = run_binary_in_sandbox(&binary, &sandbox, filename)?;
                    if snapshot != second {
                        let detail = describe_run_difference(&snapshot, &second);
                        let _ = fs::remove_file(&binary);
                        let _ = fs::remove_dir_all(&sandbox);
                        return Err(format!(
                            "{} [{}]: REPRO_CHECK(run_same_sandbox) failed: {}",
                            filename, opt_flag, detail
                        ));
                    }
                }
            }
        }
        let _ = fs::remove_file(&binary);
        let _ = fs::remove_dir_all(&sandbox);

        // IR shape assertions: only at -O0, where the IR is
        // stable. Optimization passes (mem2reg, LICM, CSE, etc.)
        // erase the very shape we want to pin, so running these
        // at -O1+ would always fail. The runtime CHECKs above
        // continue to run at every level.
        if !ir_checks.is_empty() && opt_flag == "-O0" {
            let ir_dest = unique_temp_path("test_ir", stem, "o0", ".txt");
            let ir_compile = Command::new(compiler)
                .args([
                    source.to_str().unwrap(),
                    "-O0",
                    "--emit-ir",
                    "-o",
                    ir_dest.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| format!("{}: cannot run --emit-ir: {}", filename, e))?;
            if !ir_compile.status.success() {
                let stderr = String::from_utf8_lossy(&ir_compile.stderr);
                return Err(format!(
                    "{} [{}]: --emit-ir compilation failed:\n{}",
                    filename, opt_flag, stderr,
                ));
            }
            let ir_text = fs::read_to_string(&ir_dest)
                .map_err(|e| format!("{}: cannot read IR: {}", filename, e))?;
            let _ = fs::remove_file(&ir_dest);
            match_ir_checks(&ir_checks, &ir_text, &label)?;
        }

        if !asm_checks.is_empty() {
            let asm_dest = unique_temp_path("test_asm", stem, level, ".s");
            let asm_compile = Command::new(compiler)
                .args([
                    source.to_str().unwrap(),
                    opt_flag,
                    "-S",
                    "-o",
                    asm_dest.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| format!("{}: cannot run -S: {}", filename, e))?;
            if !asm_compile.status.success() {
                let stderr = String::from_utf8_lossy(&asm_compile.stderr);
                return Err(format!(
                    "{} [{}]: -S compilation failed:\n{}",
                    filename, opt_flag, stderr,
                ));
            }
            let asm_text = fs::read_to_string(&asm_dest)
                .map_err(|e| format!("{}: cannot read assembly: {}", filename, e))?;
            let _ = fs::remove_file(&asm_dest);
            match_asm_checks(&asm_checks, &asm_text, &label)?;
        }

        run_opt_eq_rules(
            compiler,
            source,
            opt_flag,
            filename,
            &snapshot,
            &opt_eq_rules,
        )?;

        if let Some(triangulation) = &phase_triangulation {
            let declared_runtime_paths = collect_declared_runtime_paths(
                &file_checks,
                &file_presence_checks,
                &file_line_count_checks,
                &file_rerun_mode_checks,
                file_set_exact.as_ref(),
            );
            run_phase_triangulation(
                compiler,
                source,
                opt_flag,
                filename,
                triangulation,
                &declared_runtime_paths,
            )?;
        }

        Ok(())
    };

    let result = inner();
    match (xfail_reason, result) {
        (None, Ok(())) => TestOutcome::Pass,
        (None, Err(e)) => TestOutcome::Fail(e),
        (Some(reason), Err(e)) => TestOutcome::Xfail(format!("{}: {}", reason, e)),
        (Some(reason), Ok(())) => TestOutcome::Xpass(format!(
            "{} [{}]: marked XFAIL ({}) but unexpectedly passed — \
             remove the XFAIL annotation",
            filename, opt_flag, reason,
        )),
    }
}

/// Discover the test programs and run each at every supported opt level.
/// This enforces the correctness invariant: same source must produce
/// the same output regardless of optimization level.
fn run_all_at(opt_flag: &str) -> Result<(), String> {
    let compiler = find_compiler();
    let test_dir = find_test_programs();

    let mut sources: Vec<PathBuf> = fs::read_dir(&test_dir)
        .expect("cannot read test_programs/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_test_program_source(p))
        .collect();
    sources.sort();

    assert!(
        !sources.is_empty(),
        "no Fortran sources found in test_programs/"
    );

    let mut failures = Vec::new();
    let mut passed = 0;
    let mut xfailed = 0;

    for source in &sources {
        let name = source.file_name().unwrap().to_str().unwrap();
        match run_test(&compiler, source, opt_flag) {
            TestOutcome::Pass => {
                passed += 1;
                eprintln!("  PASS  [{}]: {}", opt_flag, name);
            }
            TestOutcome::Xfail(detail) => {
                xfailed += 1;
                // Print the first line of the detail so we know what
                // the underlying failure looked like, in case the bug
                // class shifts.
                let one_line = detail.lines().next().unwrap_or("");
                eprintln!("  XFAIL [{}]: {} — {}", opt_flag, name, one_line);
            }
            TestOutcome::Xpass(msg) => {
                eprintln!("  XPASS [{}]: {}", opt_flag, name);
                failures.push(msg);
            }
            TestOutcome::Fail(msg) => {
                eprintln!("  FAIL  [{}]: {}", opt_flag, name);
                failures.push(msg);
            }
        }
    }

    eprintln!(
        "\n[{}] {} passed, {} xfailed, {} failed out of {} test programs",
        opt_flag,
        passed,
        xfailed,
        failures.len(),
        sources.len(),
    );

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n"))
    }
}

#[test]
fn test_programs_end_to_end() {
    if let Err(msg) = run_all_at("-O0") {
        panic!("Test failures at -O0:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o1() {
    if let Err(msg) = run_all_at("-O1") {
        panic!("Test failures at -O1:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o2() {
    if let Err(msg) = run_all_at("-O2") {
        panic!("Test failures at -O2:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o3() {
    if let Err(msg) = run_all_at("-O3") {
        panic!("Test failures at -O3:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_os() {
    if let Err(msg) = run_all_at("-Os") {
        panic!("Test failures at -Os:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_ofast() {
    if let Err(msg) = run_all_at("-Ofast") {
        panic!("Test failures at -Ofast:\n\n{}", msg);
    }
}

#[test]
fn test_program_source_filter_accepts_fixed_form_extensions() {
    assert!(is_test_program_source(Path::new("hello.f")));
    assert!(is_test_program_source(Path::new("legacy.for")));
    assert!(is_test_program_source(Path::new("solver.ftn")));
    assert!(is_test_program_source(Path::new("main.f90")));
    assert!(!is_test_program_source(Path::new("notes.txt")));
}

/// Determinism regression: compile a program twice at -O2 and
/// require byte-identical machine code. Codegen non-determinism
/// (HashMap iteration order, stale spill-victim entries, sort
/// tie-breaking) caused this to flake during the mem2reg work; the
/// test pins the invariant going forward so any future regression
/// trips immediately instead of intermittently.
fn compile_to_asm(compiler: &Path, source: &Path, opt: &str) -> Vec<u8> {
    let asm_path = unique_temp_path(
        "det_asm",
        source.file_stem().unwrap().to_str().unwrap(),
        opt.trim_start_matches('-'),
        ".s",
    );
    let status = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt,
            "-S",
            "-o",
            asm_path.to_str().unwrap(),
        ])
        .status()
        .expect("compiler launch failed");
    assert!(status.success(), "-S compile failed");
    let bytes = fs::read(&asm_path).expect("cannot read emitted .s");
    let _ = fs::remove_file(&asm_path);
    bytes
}

#[test]
fn codegen_is_deterministic_at_o2() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("two_loops.f90");
    assert!(
        source.exists(),
        "two_loops.f90 missing — needed for determinism check"
    );

    let first = compile_to_asm(&compiler, &source, "-O2");
    let second = compile_to_asm(&compiler, &source, "-O2");
    assert_eq!(
        first, second,
        "two compilations of the same source produced different assembly — \
         determinism regression. This usually means a HashMap iteration \
         order leak in codegen."
    );
}

/// Determinism regression for programs that import module globals.
/// Audit B-3: `install_globals_as_locals` iterated a HashMap, so
/// the emitted `global_addr` instructions landed in non-deterministic
/// positions — liveness and regalloc then produced different .s
/// output. This test pins the fix for every opt level that runs a
/// register allocator.
#[test]
fn codegen_is_deterministic_with_module_globals() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("module_init.f90");
    assert!(
        source.exists(),
        "module_init.f90 missing — needed for determinism check"
    );

    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let first = compile_to_asm(&compiler, &source, opt);
        let second = compile_to_asm(&compiler, &source, opt);
        assert_eq!(
            first, second,
            "two compilations of module_init.f90 produced different assembly at {} — \
             this usually means install_globals_as_locals is iterating a HashMap \
             in non-deterministic order.",
            opt,
        );
    }
}

#[test]
fn extract_exit_code_accepts_integer_annotation() {
    let source = "! EXIT_CODE: 17\nprogram t\nend program t\n";
    assert_eq!(extract_exit_code(source, "inline.f90").unwrap(), Some(17));
}

#[test]
fn extract_error_span_accepts_line_and_column() {
    let source = "! ERROR_EXPECTED: hidden\n! ERROR_SPAN: 13:19\nprogram t\nend program t\n";
    assert_eq!(
        extract_error_span(source, "inline.f90").unwrap(),
        Some(ExpectedSpan {
            line_num: 2,
            line: 13,
            col: 19,
        })
    );
}

#[test]
fn extract_file_checks_accepts_relative_path_and_pattern() {
    let source = "! FILE_CHECK: out.txt => hello\n! FILE_NOT: out.txt => goodbye\n";
    let checks = extract_file_checks(source, "inline.f90", "! FILE_CHECK:", "! FILE_NOT:").unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0].rel_path, "out.txt");
    assert_eq!(checks[0].pattern, "hello");
    assert!(!checks[0].negative);
    assert!(checks[1].negative);
}

#[test]
fn extract_file_presence_checks_accepts_exists_and_missing() {
    let source = "! FILE_EXISTS: out.txt\n! FILE_MISSING: ghost.txt\n";
    let checks = extract_file_presence_checks(source, "inline.f90").unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0].rel_path, "out.txt");
    assert!(checks[0].should_exist);
    assert_eq!(checks[1].rel_path, "ghost.txt");
    assert!(!checks[1].should_exist);
}

#[test]
fn extract_file_line_count_checks_accepts_relative_path_and_count() {
    let source = "! FILE_LINE_COUNT: out.txt => 1000\n";
    let checks = extract_file_line_count_checks(source, "inline.f90").unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].rel_path, "out.txt");
    assert_eq!(checks[0].expected_lines, 1000);
}

#[test]
fn extract_file_rerun_mode_checks_accepts_stable_and_append() {
    let source = "! FILE_RERUN_MODE: out.txt => stable\n! FILE_RERUN_MODE: log.txt => append\n";
    let checks = extract_file_rerun_mode_checks(source, "inline.f90").unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0].rel_path, "out.txt");
    assert_eq!(checks[0].mode, FileRerunMode::Stable);
    assert_eq!(checks[1].rel_path, "log.txt");
    assert_eq!(checks[1].mode, FileRerunMode::Append);
}

#[test]
fn extract_file_set_exact_accepts_relative_paths() {
    let check = extract_file_set_exact("! FILE_SET_EXACT: out.txt, log.txt\n", "inline.f90")
        .unwrap()
        .unwrap();
    assert_eq!(check.rel_paths, vec!["log.txt", "out.txt"]);
}

#[test]
fn file_rerun_mode_matcher_accepts_strict_append_growth() {
    let checks = vec![FileRerunModeCheck {
        line_num: 1,
        rel_path: "log.txt".into(),
        mode: FileRerunMode::Append,
    }];
    let first = RunSnapshot {
        exit_code: 0,
        stdout: "7\n".into(),
        stderr: String::new(),
        files: BTreeMap::from([("log.txt".into(), b" 7\n".to_vec())]),
    };
    let second = RunSnapshot {
        exit_code: 0,
        stdout: "7\n".into(),
        stderr: String::new(),
        files: BTreeMap::from([("log.txt".into(), b" 7\n 7\n".to_vec())]),
    };

    match_file_rerun_mode_checks(&checks, &first, &second, "inline.f90 [O0]").unwrap();
}

#[test]
fn extract_repro_checks_rejects_unknown_stage() {
    let source = "! REPRO_CHECK: ir\n";
    let err = extract_repro_checks(source, "inline.f90").unwrap_err();
    assert!(err.contains("asm, obj, run, run_same_sandbox"));
}

#[test]
fn extract_repro_checks_accepts_run_same_sandbox_stage() {
    let source = "! REPRO_CHECK: run_same_sandbox\n";
    let checks = extract_repro_checks(source, "inline.f90").unwrap();
    assert_eq!(checks, vec![ReproStage::RunSameSandbox]);
}

#[test]
fn extract_opt_eq_rules_accepts_runtime_and_asm_components() {
    let source = "! OPT_EQ: O0,Os,O2 => stdout|stderr|exit|asm\n";
    let rules = extract_opt_eq_rules(source, "inline.f90").unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].opt_flags, vec!["-O0", "-Os", "-O2"]);
    assert_eq!(
        rules[0].components,
        vec![
            OptEqComponent::Stdout,
            OptEqComponent::Stderr,
            OptEqComponent::Exit,
            OptEqComponent::Asm
        ]
    );
}

#[test]
fn extract_opt_eq_rules_rejects_unknown_component() {
    let source = "! OPT_EQ: O0,O1 => ir\n";
    let err = extract_opt_eq_rules(source, "inline.f90").unwrap_err();
    assert!(err.contains("stdout, stderr, exit, or asm"));
}

#[test]
fn extract_phase_triangulation_accepts_ir_asm_obj_clean_and_repro() {
    let source = "! PHASE_TRIANGULATE: ir|asm|obj|clean|repro\n";
    let rule = extract_phase_triangulation(source, "inline.f90")
        .unwrap()
        .unwrap();
    assert_eq!(
        rule.surfaces,
        vec![
            PhaseSurface::Ir,
            PhaseSurface::Asm,
            PhaseSurface::Obj,
            PhaseSurface::Clean,
            PhaseSurface::Repro
        ]
    );
}

#[test]
fn extract_phase_triangulation_rejects_unknown_surface() {
    let source = "! PHASE_TRIANGULATE: run\n";
    let err = extract_phase_triangulation(source, "inline.f90").unwrap_err();
    assert!(err.contains("ir, asm, obj, clean, or repro"));
}

#[test]
fn extract_phase_triangulation_rejects_clean_only() {
    let source = "! PHASE_TRIANGULATE: clean|repro\n";
    let err = extract_phase_triangulation(source, "inline.f90").unwrap_err();
    assert!(err.contains("policy-only annotations"));
}

#[test]
fn extract_exit_code_rejects_multiple_annotations() {
    let source = "! EXIT_CODE: 1\n! EXIT_CODE: 2\nprogram t\nend program t\n";
    let err = extract_exit_code(source, "inline.f90").unwrap_err();
    assert!(err.contains("multiple EXIT_CODE annotations"));
}

#[test]
fn match_checks_reports_stderr_check_failures_by_name() {
    let checks = vec![Check {
        line_num: 1,
        pattern: "ERROR STOP".into(),
    }];
    let err = match_checks(
        &checks,
        "different stderr",
        "inline.f90 [O0]",
        "STDERR_CHECK",
    )
    .unwrap_err();
    assert!(err.contains("STDERR_CHECK failed"));
}

#[test]
fn diagnostic_contains_span_matches_line_and_column_fragment() {
    let stderr = "armfortas: error: 13:19: hidden is not accessible";
    assert!(diagnostic_contains_span(
        stderr,
        ExpectedSpan {
            line_num: 1,
            line: 13,
            col: 19,
        }
    ));
}

#[test]
fn stderr_and_exit_code_annotations_allow_error_stop() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_stop_status.f90");
    assert!(
        source.exists(),
        "error_stop_status.f90 missing — needed for stderr/exit-code harness coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!("error_stop_status.f90 should pass, got {:?}", other),
    }
}

#[test]
fn error_expected_and_span_match_hidden_use_only_error() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("audit6_filter_associate.f90");
    assert!(
        source.exists(),
        "audit6_filter_associate.f90 missing — needed for ERROR_SPAN coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "audit6_filter_associate.f90 should pass with ERROR_EXPECTED + ERROR_SPAN, got {:?}",
            other
        ),
    }
}

#[test]
fn file_checks_allow_file_roundtrip() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("file_io.f90");
    assert!(
        source.exists(),
        "file_io.f90 missing — needed for FILE_CHECK coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "file_io.f90 should pass with FILE_CHECK coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn file_presence_checks_allow_rewind_side_effects() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("io_rewind.f90");
    assert!(
        source.exists(),
        "io_rewind.f90 missing — needed for FILE_EXISTS/FILE_MISSING coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "io_rewind.f90 should pass with FILE_EXISTS/FILE_MISSING coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn file_set_exact_allows_rewind_single_output() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("io_rewind.f90");
    assert!(
        source.exists(),
        "io_rewind.f90 missing — needed for FILE_SET_EXACT coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "io_rewind.f90 should pass with FILE_SET_EXACT coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn file_rerun_mode_append_fixture_passes_and_keeps_append_coverage() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("io_append_log.f90");
    assert!(
        source.exists(),
        "io_append_log.f90 missing — needed for FILE_RERUN_MODE append coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "io_append_log.f90 should pass while still exercising FILE_RERUN_MODE(append), got {:?}",
            other
        ),
    }
}

#[test]
fn file_line_count_and_same_sandbox_repro_allow_flush_stress() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("io_flush_stress.f90");
    assert!(
        source.exists(),
        "io_flush_stress.f90 missing — needed for FILE_LINE_COUNT and run_same_sandbox coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "io_flush_stress.f90 should pass with FILE_LINE_COUNT and REPRO_CHECK(run_same_sandbox), got {:?}",
            other
        ),
    }
}

#[test]
fn repro_checks_allow_hello_stage_repro() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("hello.f90");
    assert!(
        source.exists(),
        "hello.f90 missing — needed for REPRO_CHECK coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "hello.f90 should pass with REPRO_CHECK coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn scalar_real_array_integer_bulk_fill_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("scalar_real_array_integer_bulk_fill.f90");
    assert!(
        source.exists(),
        "scalar_real_array_integer_bulk_fill.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "scalar_real_array_integer_bulk_fill.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn complex_section_assign_preserves_imag_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_section_assign_preserves_imag.f90");
    assert!(
        source.exists(),
        "complex_section_assign_preserves_imag.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "complex_section_assign_preserves_imag.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn procedure_dummy_interface_scope_hidden_lengths_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("procedure_dummy_interface_scope_hidden_lengths.f90");
    assert!(
        source.exists(),
        "procedure_dummy_interface_scope_hidden_lengths.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "procedure_dummy_interface_scope_hidden_lengths.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn parenthesized_concat_char_actual_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("parenthesized_concat_char_actual.f90");
    assert!(
        source.exists(),
        "parenthesized_concat_char_actual.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "parenthesized_concat_char_actual.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn defined_assignment_logical_kind_allocatable_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("defined_assignment_logical_kind_allocatable.f90");
    assert!(
        source.exists(),
        "defined_assignment_logical_kind_allocatable.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "defined_assignment_logical_kind_allocatable.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn host_assumed_len_closure_forward_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("host_assumed_len_closure_forward.f90");
    assert!(
        source.exists(),
        "host_assumed_len_closure_forward.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "host_assumed_len_closure_forward.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn merge_scalar_sources_array_mask_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("merge_scalar_sources_array_mask.f90");
    assert!(
        source.exists(),
        "merge_scalar_sources_array_mask.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "merge_scalar_sources_array_mask.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn complex_merge_element_zero_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_merge_element_zero.f90");
    assert!(source.exists(), "complex_merge_element_zero.f90 missing");

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "complex_merge_element_zero.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn format_g0_nonfinite_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("format_g0_nonfinite.f90");
    assert!(source.exists(), "format_g0_nonfinite.f90 missing");

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "format_g0_nonfinite.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn formatted_write_g0_iostat_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("formatted_write_g0_iostat.f90");
    assert!(source.exists(), "formatted_write_g0_iostat.f90 missing");

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "formatted_write_g0_iostat.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn generic_private_rename_derived_result_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("generic_private_rename_derived_result.f90");
    assert!(
        source.exists(),
        "generic_private_rename_derived_result.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "generic_private_rename_derived_result.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn elemental_generic_array_expr_actual_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("elemental_generic_array_expr_actual.f90");
    assert!(
        source.exists(),
        "elemental_generic_array_expr_actual.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "elemental_generic_array_expr_actual.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn submodule_use_generic_sqrt_diag_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("submodule_use_generic_sqrt_diag.f90");
    assert!(
        source.exists(),
        "submodule_use_generic_sqrt_diag.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "submodule_use_generic_sqrt_diag.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn block_use_generic_string_function_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("block_use_generic_string_function.f90");
    assert!(
        source.exists(),
        "block_use_generic_string_function.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "block_use_generic_string_function.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn generic_len_result_character_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("generic_len_result_character.f90");
    assert!(source.exists(), "generic_len_result_character.f90 missing");

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "generic_len_result_character.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn list_directed_real_implicit_exponent_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("list_directed_real_implicit_exponent.f90");
    assert!(
        source.exists(),
        "list_directed_real_implicit_exponent.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "list_directed_real_implicit_exponent.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn complex_abs_array_scalar_compare_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_abs_array_scalar_compare.f90");
    assert!(
        source.exists(),
        "complex_abs_array_scalar_compare.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "complex_abs_array_scalar_compare.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn assumed_size_param_constructor_pack_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("assumed_size_param_constructor_pack.f90");
    assert!(
        source.exists(),
        "assumed_size_param_constructor_pack.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "assumed_size_param_constructor_pack.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn fixed_char_alloc_array_constructor_len_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("fixed_char_alloc_array_constructor_len.f90");
    assert!(
        source.exists(),
        "fixed_char_alloc_array_constructor_len.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "fixed_char_alloc_array_constructor_len.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn defined_operator_char_array_element_actual_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("defined_operator_char_array_element_actual.f90");
    assert!(
        source.exists(),
        "defined_operator_char_array_element_actual.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "defined_operator_char_array_element_actual.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn defined_assignment_derived_operator_result_fixture_passes_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("defined_assignment_derived_operator_result.f90");
    assert!(
        source.exists(),
        "defined_assignment_derived_operator_result.f90 missing"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "defined_assignment_derived_operator_result.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn opt_eq_annotations_allow_hello_cross_opt_invariant() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("hello.f90");
    assert!(
        source.exists(),
        "hello.f90 missing — needed for OPT_EQ coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "hello.f90 should pass with OPT_EQ coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn pack_zero_size_mask_expr_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("pack_zero_size_mask_expr.f90");
    assert!(source.exists(), "pack_zero_size_mask_expr.f90 missing");

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "pack_zero_size_mask_expr.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn derived_alloc_array_component_deep_copy_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("derived_alloc_array_component_deep_copy.f90");
    assert!(
        source.exists(),
        "derived_alloc_array_component_deep_copy.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "derived_alloc_array_component_deep_copy.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn allocate_array_scalar_source_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("allocate_array_scalar_source.f90");
    assert!(source.exists(), "allocate_array_scalar_source.f90 missing");

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "allocate_array_scalar_source.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn random_seed_size_put_get_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("random_seed_size_put_get.f90");
    assert!(source.exists(), "random_seed_size_put_get.f90 missing");

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "random_seed_size_put_get.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn open_module_char_parameter_file_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("open_module_char_parameter_file.f90");
    assert!(
        source.exists(),
        "open_module_char_parameter_file.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "open_module_char_parameter_file.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn open_fixed_char_filename_trim_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("open_fixed_char_filename_trim.f90");
    assert!(source.exists(), "open_fixed_char_filename_trim.f90 missing");

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "open_fixed_char_filename_trim.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn class_star_character_select_type_message_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("class_star_character_select_type_message.f90");
    assert!(
        source.exists(),
        "class_star_character_select_type_message.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "class_star_character_select_type_message.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn class_star_real_kind_select_type_state_message_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("class_star_real_kind_select_type_state_message.f90");
    assert!(
        source.exists(),
        "class_star_real_kind_select_type_state_message.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "class_star_real_kind_select_type_state_message.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn class_star_rank1_complex_select_type_state_message_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("class_star_rank1_complex_select_type_state_message.f90");
    assert!(
        source.exists(),
        "class_star_rank1_complex_select_type_state_message.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "class_star_rank1_complex_select_type_state_message.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn inquire_positional_unit_capabilities_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("inquire_positional_unit_capabilities.f90");
    assert!(
        source.exists(),
        "inquire_positional_unit_capabilities.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "inquire_positional_unit_capabilities.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn pack_strided_row_section_mask_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("pack_strided_row_section_mask.f90");
    assert!(source.exists(), "pack_strided_row_section_mask.f90 missing");

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "pack_strided_row_section_mask.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn negative_stride_section_overlap_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("negative_stride_section_overlap.f90");
    assert!(
        source.exists(),
        "negative_stride_section_overlap.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "negative_stride_section_overlap.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn vector_subscript_array_rhs_scatter_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("vector_subscript_array_rhs_scatter.f90");
    assert!(
        source.exists(),
        "vector_subscript_array_rhs_scatter.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "vector_subscript_array_rhs_scatter.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn rank_remap_strided_section_copyin_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("rank_remap_strided_section_copyin.f90");
    assert!(
        source.exists(),
        "rank_remap_strided_section_copyin.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "rank_remap_strided_section_copyin.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn maxval_minval_abs_dim_reduction_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("maxval_minval_abs_dim_reduction.f90");
    assert!(
        source.exists(),
        "maxval_minval_abs_dim_reduction.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "maxval_minval_abs_dim_reduction.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn array_constructor_whole_array_exprs_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("array_constructor_whole_array_exprs.f90");
    assert!(
        source.exists(),
        "array_constructor_whole_array_exprs.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "array_constructor_whole_array_exprs.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn mixed_numeric_array_binary_exprs_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("mixed_numeric_array_binary_exprs.f90");
    assert!(
        source.exists(),
        "mixed_numeric_array_binary_exprs.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "mixed_numeric_array_binary_exprs.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn matmul_transpose_inline_rank2_array_expr_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("matmul_transpose_inline_rank2_array_expr.f90");
    assert!(
        source.exists(),
        "matmul_transpose_inline_rank2_array_expr.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "matmul_transpose_inline_rank2_array_expr.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn real_alloc_result_to_complex_fixed_array_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("real_alloc_result_to_complex_fixed_array.f90");
    assert!(
        source.exists(),
        "real_alloc_result_to_complex_fixed_array.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "real_alloc_result_to_complex_fixed_array.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn sibling_parameter_reshape_shape_scope_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("sibling_parameter_reshape_shape_scope.f90");
    assert!(
        source.exists(),
        "sibling_parameter_reshape_shape_scope.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "sibling_parameter_reshape_shape_scope.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn block_kind_intrinsic_source_alloc_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("block_kind_intrinsic_source_alloc.f90");
    assert!(
        source.exists(),
        "block_kind_intrinsic_source_alloc.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "block_kind_intrinsic_source_alloc.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn explicit_shape_sequence_section_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("explicit_shape_sequence_section.f90");
    assert!(
        source.exists(),
        "explicit_shape_sequence_section.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "explicit_shape_sequence_section.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn elemental_conversion_array_constructor_alloc_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("elemental_conversion_array_constructor_alloc.f90");
    assert!(
        source.exists(),
        "elemental_conversion_array_constructor_alloc.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "elemental_conversion_array_constructor_alloc.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn parameter_array_assumed_size_initializer_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("parameter_array_assumed_size_initializer.f90");
    assert!(
        source.exists(),
        "parameter_array_assumed_size_initializer.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "parameter_array_assumed_size_initializer.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn logical_kind_array_scalar_broadcast_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("logical_kind_array_scalar_broadcast.f90");
    assert!(
        source.exists(),
        "logical_kind_array_scalar_broadcast.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "logical_kind_array_scalar_broadcast.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn entity_character_length_declarators_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("entity_character_length_declarators.f90");
    assert!(
        source.exists(),
        "entity_character_length_declarators.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "entity_character_length_declarators.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn complex_vector_subscript_section_expr_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_vector_subscript_section_expr.f90");
    assert!(
        source.exists(),
        "complex_vector_subscript_section_expr.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "complex_vector_subscript_section_expr.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn complex_module_param_reshape_exprs_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_module_param_reshape_exprs.f90");
    assert!(
        source.exists(),
        "complex_module_param_reshape_exprs.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "complex_module_param_reshape_exprs.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn complex_zero_integer_power_lapack_shift_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_zero_integer_power_lapack_shift.f90");
    assert!(
        source.exists(),
        "complex_zero_integer_power_lapack_shift.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "complex_zero_integer_power_lapack_shift.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn complex_reshape_integer_constructor_zero_imag_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_reshape_integer_constructor_zero_imag.f90");
    assert!(
        source.exists(),
        "complex_reshape_integer_constructor_zero_imag.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "complex_reshape_integer_constructor_zero_imag.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn complex_dp_parameter_zero_compare_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("complex_dp_parameter_zero_compare.f90");
    assert!(
        source.exists(),
        "complex_dp_parameter_zero_compare.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "complex_dp_parameter_zero_compare.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn pointer_result_forward_no_stack_copy_fixture_passes_all_opts() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("pointer_result_forward_no_stack_copy.f90");
    assert!(
        source.exists(),
        "pointer_result_forward_no_stack_copy.f90 missing"
    );

    for opt_flag in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        match run_test(&compiler, &source, opt_flag) {
            TestOutcome::Pass => {}
            other => panic!(
                "pointer_result_forward_no_stack_copy.f90 should pass at {}, got {:?}",
                opt_flag, other
            ),
        }
    }
}

#[test]
fn phase_triangulation_allows_function_call_pipeline_surfaces() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("function_call.f90");
    assert!(
        source.exists(),
        "function_call.f90 missing — needed for PHASE_TRIANGULATE coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "function_call.f90 should pass with PHASE_TRIANGULATE coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn phase_triangulation_repro_keeps_function_call_compile_surfaces_stable() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("function_call.f90");
    assert!(
        source.exists(),
        "function_call.f90 missing — needed for PHASE_TRIANGULATE(repro) coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "function_call.f90 should pass with PHASE_TRIANGULATE(repro) coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn phase_triangulation_clean_keeps_compile_phases_free_of_runtime_files() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("io_flush_stress.f90");
    assert!(
        source.exists(),
        "io_flush_stress.f90 missing — needed for PHASE_TRIANGULATE(clean) coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "io_flush_stress.f90 should pass with PHASE_TRIANGULATE(clean) coverage, got {:?}",
            other
        ),
    }
}

#[test]
fn split_multifile_segments_parses_markers() {
    let src = "\
! CHECK: 42
! MULTIFILE_LINK: mod.f90 main.f90
!--- file: mod.f90
module m
  integer :: x = 42
end module
!--- file: main.f90
program p
  use m
  print *, x
end program
";
    let segs = split_multifile_segments(src).unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].name, "mod.f90");
    assert!(segs[0].source.contains("module m"));
    assert_eq!(segs[1].name, "main.f90");
    assert!(segs[1].source.contains("program p"));
}

#[test]
fn split_multifile_segments_returns_none_for_single_file() {
    let src = "program t\n  print *, 1\nend program\n";
    assert!(split_multifile_segments(src).is_none());
}

#[test]
fn extract_multifile_link_parses_order() {
    let src = "! MULTIFILE_LINK: a.f90 b.f90 c.f90\n! CHECK: ok\n";
    let order = extract_multifile_link(src).unwrap();
    assert_eq!(order, vec!["a.f90", "b.f90", "c.f90"]);
}

#[test]
fn multifile_basic_module_runs_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("multifile_basic_module.f90");
    assert!(source.exists(), "multifile_basic_module.f90 missing");
    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "multifile_basic_module.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn multifile_three_modules_runs_at_o0() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("multifile_three_modules.f90");
    assert!(source.exists(), "multifile_three_modules.f90 missing");
    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "multifile_three_modules.f90 should pass at -O0, got {:?}",
            other
        ),
    }
}

#[test]
fn multifile_error_circular_direct_detected() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_circular_use_direct.f90");
    assert!(source.exists(), "error_circular_use_direct.f90 missing");
    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "circular use direct should pass (ERROR_EXPECTED match), got {:?}",
            other
        ),
    }
}

#[test]
fn multifile_error_circular_direct_detected_at_o1() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_circular_use_direct.f90");
    assert!(source.exists(), "error_circular_use_direct.f90 missing");
    match run_test(&compiler, &source, "-O1") {
        TestOutcome::Pass => {}
        other => panic!(
            "circular use direct should pass (ERROR_EXPECTED match) at -O1, got {:?}",
            other
        ),
    }
}

#[test]
fn multifile_error_circular_indirect_detected() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_circular_use_indirect.f90");
    assert!(source.exists(), "error_circular_use_indirect.f90 missing");
    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "circular use indirect should pass (ERROR_EXPECTED match), got {:?}",
            other
        ),
    }
}

#[test]
fn multifile_error_circular_indirect_detected_at_o1() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_circular_use_indirect.f90");
    assert!(source.exists(), "error_circular_use_indirect.f90 missing");
    match run_test(&compiler, &source, "-O1") {
        TestOutcome::Pass => {}
        other => panic!(
            "circular use indirect should pass (ERROR_EXPECTED match) at -O1, got {:?}",
            other
        ),
    }
}

#[test]
fn single_file_module_program_does_not_leave_root_amod() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("module_global_host_assoc.f90");
    assert!(source.exists(), "module_global_host_assoc.f90 missing");
    let leaked = PathBuf::from("module_global_host_assoc_mod.amod");
    let _ = fs::remove_file(&leaked);
    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!(
            "module_global_host_assoc.f90 should pass at -O0 without leaking .amod, got {:?}",
            other
        ),
    }
    assert!(
        !leaked.exists(),
        "single-file run_test should not leak {} into the repo root",
        leaked.display()
    );
}
