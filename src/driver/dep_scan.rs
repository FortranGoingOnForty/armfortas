//! Dependency scanner — quick-scan Fortran source files to extract
//! MODULE definitions and USE dependencies without a full parse.
//!
//! Used by the multi-source driver mode to determine compilation
//! order via topological sort.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Information extracted from a single source file.
#[derive(Debug)]
pub struct FileDeps {
    pub path: PathBuf,
    /// Module names this file defines (lowercase).
    pub defines: Vec<String>,
    /// Module names this file USEs (lowercase).
    pub uses: Vec<String>,
}

/// Scan a source file for MODULE and USE statements.
/// Uses a simple line-by-line keyword scan — no lexer or parser needed.
pub fn scan_file(path: &Path) -> Result<FileDeps, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;

    let mut defines = Vec::new();
    let mut uses = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim().to_lowercase();
        // Skip comments and empty lines.
        if trimmed.starts_with('!') || trimmed.is_empty() { continue; }

        // MODULE <name> — but not "module procedure" or "module function"
        if trimmed.starts_with("module ") {
            let rest = trimmed[7..].trim();
            if rest.starts_with("procedure") || rest.starts_with("function")
                || rest.starts_with("subroutine")
            {
                continue;
            }
            // Extract the module name (first identifier after "module").
            if let Some(name) = rest.split_whitespace().next() {
                let clean = name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !clean.is_empty() {
                    defines.push(clean.to_string());
                }
            }
        }

        // USE <name> [, ...]
        if trimmed.starts_with("use ") || trimmed.starts_with("use,") {
            let rest = if trimmed.starts_with("use,") {
                // USE, intrinsic :: name
                if let Some(idx) = trimmed.find("::") {
                    trimmed[idx + 2..].trim()
                } else {
                    &trimmed[4..]
                }
            } else {
                trimmed[4..].trim()
            };
            if let Some(name) = rest.split(|c: char| c == ',' || c == ':' || c.is_whitespace()).next() {
                let clean = name.trim();
                if !clean.is_empty() && clean != "only" {
                    // Skip intrinsic modules — they don't need .amod files.
                    if !is_intrinsic_module(clean) {
                        uses.push(clean.to_string());
                    }
                }
            }
        }
    }

    // Deduplicate.
    uses.sort();
    uses.dedup();
    defines.sort();
    defines.dedup();

    Ok(FileDeps { path: path.to_path_buf(), defines, uses })
}

fn is_intrinsic_module(name: &str) -> bool {
    matches!(name,
        "iso_c_binding" | "iso_fortran_env" |
        "ieee_arithmetic" | "ieee_exceptions" | "ieee_features"
    )
}

/// Determine compilation order for a set of source files.
/// Returns the files in topological order (dependencies first).
/// Errors on circular dependencies.
pub fn resolve_compilation_order(files: &[FileDeps]) -> Result<Vec<usize>, String> {
    // Build: module_name → file_index that defines it.
    let mut module_to_file: HashMap<&str, usize> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for def in &f.defines {
            module_to_file.insert(def.as_str(), i);
        }
    }

    // Build adjacency list: file i depends on file j if i USEs a module defined by j.
    let n = files.len();
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n]; // j → [files that depend on j]

    for (i, f) in files.iter().enumerate() {
        for used in &f.uses {
            if let Some(&j) = module_to_file.get(used.as_str()) {
                if i != j {
                    dependents[j].push(i);
                    in_degree[i] += 1;
                }
            }
            // If used module is not defined by any file in the set,
            // it's either intrinsic or external — skip (not an error
            // here; the compiler will diagnose at USE-resolution time).
        }
    }

    // Kahn's algorithm for topological sort.
    let mut queue: VecDeque<usize> = VecDeque::new();
    for i in 0..n {
        if in_degree[i] == 0 {
            queue.push_back(i);
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(j) = queue.pop_front() {
        order.push(j);
        for &dep in &dependents[j] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                queue.push_back(dep);
            }
        }
    }

    if order.len() != n {
        // Cycle detected — find the modules involved.
        let cycle_files: Vec<&str> = (0..n)
            .filter(|i| in_degree[*i] > 0)
            .map(|i| files[i].path.file_name().unwrap_or_default().to_str().unwrap_or("?"))
            .collect();
        return Err(format!(
            "circular module dependency detected among: {}",
            cycle_files.join(", ")
        ));
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_simple_module() {
        let dir = std::env::temp_dir().join("dep_scan_test_1");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mod.f90");
        std::fs::write(&f, "module mymod\n  use other_mod\n  implicit none\nend module\n").unwrap();
        let deps = scan_file(&f).unwrap();
        assert_eq!(deps.defines, vec!["mymod"]);
        assert_eq!(deps.uses, vec!["other_mod"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topo_sort_chain() {
        let files = vec![
            FileDeps { path: "c.f90".into(), defines: vec!["c".into()], uses: vec![] },
            FileDeps { path: "b.f90".into(), defines: vec!["b".into()], uses: vec!["c".into()] },
            FileDeps { path: "a.f90".into(), defines: vec!["a".into()], uses: vec!["b".into()] },
        ];
        let order = resolve_compilation_order(&files).unwrap();
        // c must come before b, b before a.
        let pos_c = order.iter().position(|&i| i == 0).unwrap();
        let pos_b = order.iter().position(|&i| i == 1).unwrap();
        let pos_a = order.iter().position(|&i| i == 2).unwrap();
        assert!(pos_c < pos_b);
        assert!(pos_b < pos_a);
    }

    #[test]
    fn topo_sort_cycle() {
        let files = vec![
            FileDeps { path: "a.f90".into(), defines: vec!["a".into()], uses: vec!["b".into()] },
            FileDeps { path: "b.f90".into(), defines: vec!["b".into()], uses: vec!["a".into()] },
        ];
        let err = resolve_compilation_order(&files).unwrap_err();
        assert!(err.contains("circular"), "expected cycle error, got: {}", err);
    }

    #[test]
    fn topo_sort_diamond() {
        let files = vec![
            FileDeps { path: "d.f90".into(), defines: vec!["d".into()], uses: vec![] },
            FileDeps { path: "b.f90".into(), defines: vec!["b".into()], uses: vec!["d".into()] },
            FileDeps { path: "c.f90".into(), defines: vec!["c".into()], uses: vec!["d".into()] },
            FileDeps { path: "a.f90".into(), defines: vec!["a".into()], uses: vec!["b".into(), "c".into()] },
        ];
        let order = resolve_compilation_order(&files).unwrap();
        let pos_d = order.iter().position(|&i| i == 0).unwrap();
        let pos_a = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_d < pos_a);
        assert_eq!(order.len(), 4);
    }
}
