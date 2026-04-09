//! End-to-end test harness for ARMFORTAS.
//!
//! Discovers `.f90` files in `test_programs/`, compiles each with `armfortas`,
//! runs the binary, and checks stdout against `! CHECK:` annotations.
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
//! ## FILE_CHECK / FILE_NOT annotations
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
//!
//! The linked runtime path is the anchor. If the program runs correctly but
//! one of the requested extra surfaces fails to compile or produces an empty
//! artifact, the test still fails. This is how the harness starts relating
//! linked execution to `--emit-ir`, `-S`, and `-c` instead of treating them as
//! isolated one-off checks.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReproStage {
    Asm,
    Obj,
    Run,
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
            other => {
                return Err(format!(
                    "{}:{}: REPRO_CHECK must be one of asm, obj, run; got '{}'",
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
        "O0" | "O1" | "O2" | "O3" | "Ofast" => Ok(format!("-{}", trimmed)),
        other => Err(format!(
            "{}:{}: OPT_EQ only supports O0, O1, O2, O3, or Ofast; got '{}'",
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
                other => {
                    return Err(format!(
                        "{}:{}: PHASE_TRIANGULATE surfaces must be ir, asm, or obj; got '{}'",
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
    // Look in cargo's target directory.
    let candidates = ["target/debug/armfortas", "target/release/armfortas"];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return p;
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

fn compile_stage_bytes(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    stage: ReproStage,
) -> Result<Vec<u8>, String> {
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let (kind, ext, extra_args): (&str, &str, &[&str]) = match stage {
        ReproStage::Asm => ("asm", ".s", &["-S"]),
        ReproStage::Obj => ("obj", ".o", &["-c"]),
        ReproStage::Run => unreachable!("run reproducibility uses runtime snapshots"),
    };
    let out = unique_temp_path(kind, stem, level, ext);
    let compile = Command::new(compiler)
        .args([source.to_str().unwrap(), opt_flag])
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
    Ok(bytes)
}

fn compile_and_run_snapshot(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    filename: &str,
) -> Result<RunSnapshot, String> {
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let binary = unique_temp_path("test_bin", stem, level, "");
    let sandbox = unique_temp_path("test_sandbox", stem, &format!("{}_opt_eq", level), "");

    let compile = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt_flag,
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("{}: cannot run compiler: {}", filename, e))?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
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
    let _ = fs::remove_dir_all(&sandbox);
    Ok(snapshot)
}

fn compile_ir_text(
    compiler: &Path,
    source: &Path,
    opt_flag: &str,
    filename: &str,
) -> Result<String, String> {
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let out = unique_temp_path("test_ir", stem, level, ".txt");
    let compile = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt_flag,
            "--emit-ir",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("{}: cannot run --emit-ir: {}", filename, e))?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        let _ = fs::remove_file(&out);
        return Err(format!(
            "{} [{}]: --emit-ir compilation failed:\n{}",
            filename, opt_flag, stderr
        ));
    }
    let text =
        fs::read_to_string(&out).map_err(|e| format!("{}: cannot read IR: {}", filename, e))?;
    let _ = fs::remove_file(&out);
    Ok(text)
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
) -> Result<(), String> {
    for surface in &triangulation.surfaces {
        match surface {
            PhaseSurface::Ir => {
                let ir_text = compile_ir_text(compiler, source, opt_flag, filename)?;
                if ir_text.trim().is_empty() {
                    return Err(format!(
                        "{}:{}: PHASE_TRIANGULATE({}) produced empty IR output at {}",
                        filename,
                        triangulation.line_num,
                        render_phase_surfaces(&triangulation.surfaces),
                        opt_flag,
                    ));
                }
            }
            PhaseSurface::Asm => {
                let asm = compile_stage_bytes(compiler, source, opt_flag, ReproStage::Asm)?;
                if asm.is_empty() {
                    return Err(format!(
                        "{}:{}: PHASE_TRIANGULATE({}) produced empty assembly output at {}",
                        filename,
                        triangulation.line_num,
                        render_phase_surfaces(&triangulation.surfaces),
                        opt_flag,
                    ));
                }
            }
            PhaseSurface::Obj => {
                let obj = compile_stage_bytes(compiler, source, opt_flag, ReproStage::Obj)?;
                if obj.is_empty() {
                    return Err(format!(
                        "{}:{}: PHASE_TRIANGULATE({}) produced empty object output at {}",
                        filename,
                        triangulation.line_num,
                        render_phase_surfaces(&triangulation.surfaces),
                        opt_flag,
                    ));
                }
            }
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
            "{}: no CHECK / STDERR_CHECK / EXIT_CODE / IR_CHECK / ASM_CHECK / FILE_CHECK / REPRO_CHECK / XFAIL / ERROR_EXPECTED / ERROR_SPAN annotations",
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

    // Try the compile/run/check pipeline. Any failure path returns
    // an Err with a message; success returns Ok.
    let inner = || -> Result<(), String> {
        // Use a per-(file,level) binary path so concurrent jobs
        // and successive runs at different levels don't stomp each other.
        let stem = source.file_stem().unwrap().to_str().unwrap();
        let level = opt_flag.trim_start_matches('-');
        let binary = unique_temp_path("test_bin", stem, level, "");

        let compile = Command::new(compiler)
            .args([
                source.to_str().unwrap(),
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
                let _ = fs::remove_file(&binary);
                return Err(format!(
                    "{} [{}]: ERROR_EXPECTED({}) but compilation succeeded",
                    filename, opt_flag, expected,
                ));
            }
            let stderr = String::from_utf8_lossy(&compile.stderr);
            if !stderr.contains(expected.as_str()) {
                return Err(format!(
                    "{} [{}]: ERROR_EXPECTED({}) but stderr did not contain it.\n\
                     Full stderr:\n{}",
                    filename, opt_flag, expected, stderr,
                ));
            }
            if let Some(expected_span) = error_span {
                if !diagnostic_contains_span(&stderr, expected_span) {
                    return Err(format!(
                        "{} [{}]: ERROR_SPAN({}:{}) but stderr did not contain that location.\n\
                         Full stderr:\n{}",
                        filename, opt_flag, expected_span.line, expected_span.col, stderr,
                    ));
                }
            }
            return Ok(());
        }

        if !compile.status.success() {
            let stderr = String::from_utf8_lossy(&compile.stderr);
            return Err(format!(
                "{} [{}]: compilation failed:\n{}",
                filename, opt_flag, stderr,
            ));
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
            run_phase_triangulation(compiler, source, opt_flag, filename, triangulation)?;
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
        .filter(|p| p.extension().map(|e| e == "f90").unwrap_or(false))
        .collect();
    sources.sort();

    assert!(!sources.is_empty(), "no .f90 files found in test_programs/");

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
fn test_programs_end_to_end_ofast() {
    if let Err(msg) = run_all_at("-Ofast") {
        panic!("Test failures at -Ofast:\n\n{}", msg);
    }
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
fn extract_repro_checks_rejects_unknown_stage() {
    let source = "! REPRO_CHECK: ir\n";
    let err = extract_repro_checks(source, "inline.f90").unwrap_err();
    assert!(err.contains("asm, obj, run"));
}

#[test]
fn extract_opt_eq_rules_accepts_runtime_and_asm_components() {
    let source = "! OPT_EQ: O0,O1,O2 => stdout|stderr|exit|asm\n";
    let rules = extract_opt_eq_rules(source, "inline.f90").unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].opt_flags, vec!["-O0", "-O1", "-O2"]);
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
fn extract_phase_triangulation_accepts_ir_asm_obj() {
    let source = "! PHASE_TRIANGULATE: ir|asm|obj\n";
    let rule = extract_phase_triangulation(source, "inline.f90")
        .unwrap()
        .unwrap();
    assert_eq!(
        rule.surfaces,
        vec![PhaseSurface::Ir, PhaseSurface::Asm, PhaseSurface::Obj]
    );
}

#[test]
fn extract_phase_triangulation_rejects_unknown_surface() {
    let source = "! PHASE_TRIANGULATE: run\n";
    let err = extract_phase_triangulation(source, "inline.f90").unwrap_err();
    assert!(err.contains("ir, asm, or obj"));
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
