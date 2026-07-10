//! fortsh module graph smoke test.
//!
//! Scans all 55 fortsh source files for MODULE/USE statements,
//! builds the dependency graph using armfortas's dep_scan module,
//! resolves topological compilation order, and attempts to compile
//! each file to .o in order.
//!
//! This is the ultimate integration test for the module system —
//! if it works on a real 55-module project, the module system
//! can handle real-world dependency chains.
//!
//! NOTE: This test requires the fortsh repo to be checked out at
//! ~/Documents/GithubOrgs/FortranGoingOnForty/fortsh. If absent,
//! the test is silently skipped.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FORTSH_COMPILED_FLOOR: usize = 14;

fn find_compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn find_fortsh() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let candidates = [
        PathBuf::from(&home).join("Documents/GithubOrgs/FortranGoingOnForty/fortsh/src"),
        PathBuf::from("../fortsh/src"),
    ];
    for c in &candidates {
        if c.is_dir() {
            return fs::canonicalize(c).ok();
        }
    }
    None
}

/// Simple line-by-line MODULE/USE scanner (same logic as dep_scan).
fn scan_file(path: &Path) -> (Vec<String>, Vec<String>) {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut defines = Vec::new();
    let mut uses = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim().to_lowercase();
        if trimmed.starts_with('!') || trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("module ") {
            let rest = rest.trim();
            if rest.starts_with("procedure")
                || rest.starts_with("function")
                || rest.starts_with("subroutine")
            {
                continue;
            }
            if let Some(name) = rest.split_whitespace().next() {
                let clean = name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !clean.is_empty() {
                    defines.push(clean.to_string());
                }
            }
        }

        if trimmed.starts_with("use ") || trimmed.starts_with("use,") {
            let rest = if let Some(stripped) = trimmed.strip_prefix("use,") {
                if let Some(idx) = trimmed.find("::") {
                    trimmed[idx + 2..].trim()
                } else {
                    stripped
                }
            } else {
                trimmed.strip_prefix("use ").unwrap().trim()
            };
            if let Some(name) = rest
                .split(|c: char| c == ',' || c == ':' || c.is_whitespace())
                .next()
            {
                let clean = name.trim();
                if !clean.is_empty() && clean != "only" {
                    uses.push(clean.to_string());
                }
            }
        }
    }

    defines.sort();
    defines.dedup();
    uses.sort();
    uses.dedup();
    (defines, uses)
}

fn is_hard_failure(stderr: &str) -> bool {
    stderr.contains("INTERNAL COMPILER ERROR")
        || stderr.contains("internal error:")
        || stderr.contains("assembler failed:")
        || stderr.contains("coerce_to_type:")
}

#[test]
fn fortsh_module_graph_resolves() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=fortsh_module_graph test=fortsh_module_graph_resolves count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let fortsh_src = match find_fortsh() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: fortsh source not found");
            return;
        }
    };

    // Collect all .f90 files.
    let mut sources: Vec<PathBuf> = Vec::new();
    fn collect_f90(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                collect_f90(&path, out);
            } else if path.extension().map(|e| e == "f90").unwrap_or(false) {
                out.push(path);
            }
        }
    }
    collect_f90(&fortsh_src, &mut sources);
    sources.sort();

    eprintln!("Found {} fortsh source files", sources.len());
    assert!(
        sources.len() >= 50,
        "expected ~55 fortsh files, found {}",
        sources.len()
    );

    // Scan each for module/use.
    let mut module_to_file: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut file_info: Vec<(PathBuf, Vec<String>, Vec<String>)> = Vec::new();

    for (i, src) in sources.iter().enumerate() {
        let (defines, uses) = scan_file(src);
        for def in &defines {
            module_to_file.insert(def.clone(), i);
        }
        file_info.push((src.clone(), defines, uses));
    }

    eprintln!("Module count: {}", module_to_file.len());

    // Build adjacency and in-degree for topo sort.
    let n = sources.len();
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];

    let intrinsic = [
        "iso_c_binding",
        "iso_fortran_env",
        "ieee_arithmetic",
        "ieee_exceptions",
        "ieee_features",
    ];
    for (i, (_path, _defs, uses)) in file_info.iter().enumerate() {
        for used in uses {
            if intrinsic.contains(&used.as_str()) {
                continue;
            }
            if let Some(&j) = module_to_file.get(used.as_str()) {
                if i != j {
                    dependents[j].push(i);
                    in_degree[i] += 1;
                }
            }
            // External modules not in the set are OK (e.g., C interop modules
            // might USE system modules). We just skip them.
        }
    }

    // Kahn's topo sort.
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for (i, &degree) in in_degree.iter().enumerate() {
        if degree == 0 {
            queue.push_back(i);
        }
    }
    let mut order = Vec::new();
    while let Some(j) = queue.pop_front() {
        order.push(j);
        for &dep in &dependents[j] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                queue.push_back(dep);
            }
        }
    }

    assert_eq!(
        order.len(),
        n,
        "circular dependency in fortsh module graph — {} files unresolved",
        n - order.len()
    );
    eprintln!("Topological order resolved ({} files)", order.len());

    // Attempt to compile each in order.
    let compiler = find_compiler();
    let build_dir = std::env::temp_dir().join(format!("afs_fortsh_graph_{}", std::process::id()));
    fs::create_dir_all(&build_dir).unwrap();

    let mut compiled = 0;
    let mut failed = Vec::new();
    let mut hard_failures = Vec::new();

    for &idx in &order {
        let src = &file_info[idx].0;
        let stem = src.file_stem().unwrap().to_str().unwrap();
        let obj = build_dir.join(format!("{}.o", stem));

        let result = Command::new(&compiler)
            .current_dir(&build_dir)
            .args([
                src.to_str().unwrap(),
                "-c",
                "-O0",
                "-DUSE_C_STRINGS",
                "-o",
                obj.to_str().unwrap(),
                &format!("-J{}", build_dir.display()),
                &format!("-I{}", build_dir.display()),
            ])
            .output()
            .expect("compiler launch failed");

        if result.status.success() {
            compiled += 1;
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);
            let summary = format!(
                "{}: {}",
                stem,
                stderr.lines().next().unwrap_or("unknown error")
            );
            if is_hard_failure(&stderr) {
                hard_failures.push(summary.clone());
            }
            failed.push(summary);
        }
    }

    let _ = fs::remove_dir_all(&build_dir);

    eprintln!(
        "fortsh module graph: {}/{} compiled, {} failed",
        compiled,
        n,
        failed.len()
    );

    if !failed.is_empty() {
        eprintln!("Failures:");
        for f in &failed {
            eprintln!("  {}", f);
        }
    }

    if !hard_failures.is_empty() {
        panic!(
            "fortsh compile should not ICE or assembler-crash; hard failures: {}",
            hard_failures.join(" | ")
        );
    }

    assert!(
        compiled >= FORTSH_COMPILED_FLOOR,
        "fortsh compiled {}/{} which regressed below the floor of {}",
        compiled,
        n,
        FORTSH_COMPILED_FLOOR
    );

    // Report counts for human tracking.
    eprintln!(
        "\nfortsh scorecard: {}/{} compiled ({:.0}%)",
        compiled,
        n,
        compiled as f64 / n as f64 * 100.0,
    );
}
