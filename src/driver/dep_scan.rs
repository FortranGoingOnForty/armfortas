//! Dependency scanner — quick-scan Fortran source files to extract
//! MODULE definitions and USE dependencies without a full parse.
//!
//! Used by the multi-source driver mode to determine compilation
//! order via topological sort.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::lexer::{SourceForm, Token, TokenKind};

/// Information extracted from a single source file.
#[derive(Debug)]
pub struct FileDeps {
    pub path: PathBuf,
    /// Module names this file defines (lowercase). For a file containing
    /// `submodule (A) C`, also includes the submodule identifier
    /// `A:C` (ancestor:name) so a nested submodule `submodule (A:C) G`
    /// can depend on it.
    pub defines: Vec<String>,
    /// Module names this file USEs (lowercase). A submodule depends on
    /// its parent (the module or parent submodule must compile first so
    /// the `.amod` exists), so the parent reference is recorded here too.
    pub uses: Vec<String>,
    /// For a submodule file: `Some((ancestor_module, parent))` where
    /// `parent` is the module name (direct submodule) or the parent
    /// submodule identifier `ancestor:parent` (nested). None for
    /// non-submodule files. Submodule objects must always be in the link
    /// set even when no consumer names them — they hold the SMP bodies.
    pub submodule_of: Option<(String, String)>,
}

fn is_identifier(token: &Token, expected: &str) -> bool {
    matches!(&token.kind, TokenKind::Identifier) && token.text.eq_ignore_ascii_case(expected)
}

fn identifier(token: &Token) -> Option<&str> {
    if matches!(&token.kind, TokenKind::Identifier) {
        Some(&token.text)
    } else {
        None
    }
}

fn statement_tokens(tokens: &[Token]) -> &[Token] {
    let end = tokens
        .iter()
        .position(|token| matches!(&token.kind, TokenKind::Comment))
        .unwrap_or(tokens.len());
    let tokens = &tokens[..end];
    if tokens
        .first()
        .is_some_and(|token| matches!(&token.kind, TokenKind::IntegerLiteral))
    {
        &tokens[1..]
    } else {
        tokens
    }
}

fn module_definition(statement: &[Token]) -> Option<String> {
    let [keyword, name] = statement else {
        return None;
    };
    if !is_identifier(keyword, "module") {
        return None;
    }
    Some(identifier(name)?.to_ascii_lowercase())
}

fn submodule_definition(statement: &[Token]) -> Option<(String, String, String)> {
    if !statement
        .first()
        .is_some_and(|token| is_identifier(token, "submodule"))
        || !statement
            .get(1)
            .is_some_and(|token| matches!(&token.kind, TokenKind::LParen))
    {
        return None;
    }
    let close = statement[2..]
        .iter()
        .position(|token| matches!(&token.kind, TokenKind::RParen))?
        + 2;
    let parent = &statement[2..close];
    let ancestor = identifier(parent.first()?)?.to_ascii_lowercase();
    let parent_ref = match parent {
        [_] => ancestor.clone(),
        [_, colon, nested]
            if matches!(&colon.kind, TokenKind::Colon)
                && matches!(&nested.kind, TokenKind::Identifier) =>
        {
            format!("{ancestor}:{}", nested.text.to_ascii_lowercase())
        }
        _ => return None,
    };
    let name = identifier(statement.get(close + 1)?)?.to_ascii_lowercase();
    Some((format!("{ancestor}:{name}"), ancestor, parent_ref))
}

fn use_dependency(statement: &[Token]) -> Option<String> {
    if !statement
        .first()
        .is_some_and(|token| is_identifier(token, "use"))
    {
        return None;
    }

    let mut index = 1;
    let mut explicit_intrinsic = false;
    if statement
        .get(index)
        .is_some_and(|token| matches!(&token.kind, TokenKind::Comma))
    {
        index += 1;
        explicit_intrinsic = identifier(statement.get(index)?)?.eq_ignore_ascii_case("intrinsic");
        index += 1;
        if !statement
            .get(index)
            .is_some_and(|token| matches!(&token.kind, TokenKind::ColonColon))
        {
            return None;
        }
        index += 1;
    } else if statement
        .get(index)
        .is_some_and(|token| matches!(&token.kind, TokenKind::ColonColon))
    {
        index += 1;
    }

    let module = identifier(statement.get(index)?)?;
    (!explicit_intrinsic).then(|| module.to_ascii_lowercase())
}

/// Preprocess and scan a source file for MODULE and USE statements.
/// Uses the source-form lexer to delimit statements without invoking the parser.
pub fn scan_file(
    path: &Path,
    config: &crate::preprocess::PreprocConfig,
) -> Result<FileDeps, String> {
    let content =
        std::fs::read(path).map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;
    let content = crate::preprocess::preprocess_bytes_for_dependency_scan(&content, config)
        .map_err(|e| e.to_string())?
        .text;
    let source_form = if config.fixed_form {
        SourceForm::FixedForm
    } else {
        SourceForm::FreeForm
    };
    let tokens = crate::lexer::tokenize_source_view(&content, 0, source_form)
        .map_err(|error| format!("cannot scan '{}': {}", path.display(), error))?;

    let mut defines = Vec::new();
    let mut uses = Vec::new();
    let mut submodule_of: Option<(String, String)> = None;

    for raw_statement in tokens.split(|token| {
        matches!(
            &token.kind,
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof
        )
    }) {
        let statement = statement_tokens(raw_statement);
        if statement.is_empty() {
            continue;
        }

        // SUBMODULE (<parent-spec>) <name> depends on its parent and defines
        // an ancestor-qualified name for any nested child submodules.
        if let Some((definition, ancestor, parent_ref)) = submodule_definition(statement) {
            uses.push(parent_ref.clone());
            defines.push(definition);
            submodule_of = Some((ancestor, parent_ref));
            continue;
        }

        // A module program-unit statement has exactly MODULE <name>. Requiring
        // that complete shape excludes MODULE PROCEDURE and prefixed module
        // functions/subroutines.
        if let Some(name) = module_definition(statement) {
            defines.push(name);
        }

        if let Some(module) = use_dependency(statement) {
            uses.push(module);
        }
    }

    // Deduplicate.
    uses.sort();
    uses.dedup();
    defines.sort();
    defines.dedup();

    Ok(FileDeps {
        path: path.to_path_buf(),
        defines,
        uses,
        submodule_of,
    })
}

/// Determine compilation order for a set of source files.
/// Returns the files in topological order (dependencies first).
/// Errors on circular dependencies.
pub fn resolve_compilation_order(files: &[FileDeps]) -> Result<Vec<usize>, String> {
    // Build: module_name → file_index that defines it.
    let mut module_to_file: HashMap<String, usize> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for def in &f.defines {
            let key = def.to_ascii_lowercase();
            if let Some(&first) = module_to_file.get(&key) {
                if first != i {
                    return Err(format!(
                        "duplicate module definition '{}': '{}' and '{}' both define it",
                        key,
                        files[first].path.display(),
                        f.path.display()
                    ));
                }
            } else {
                module_to_file.insert(key, i);
            }
        }
    }

    // Build adjacency list: file i depends on file j if i USEs a module defined by j.
    let n = files.len();
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n]; // j → [files that depend on j]

    for (i, f) in files.iter().enumerate() {
        for used in &f.uses {
            if let Some(&j) = module_to_file.get(&used.to_ascii_lowercase()) {
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
    for (i, &deg) in in_degree.iter().enumerate().take(n) {
        if deg == 0 {
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
            .map(|i| {
                files[i]
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or("?")
            })
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
        std::fs::write(
            &f,
            "module mymod\n  use other_mod\n  implicit none\nend module\n",
        )
        .unwrap();
        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.defines, vec!["mymod"]);
        assert_eq!(deps.uses, vec!["other_mod"]);
        assert_eq!(deps.submodule_of, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_honors_use_module_nature() {
        let dir = std::env::temp_dir().join("dep_scan_use_nature");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("consumer.f90");
        std::fs::write(
            &f,
            "module consumer\n  use, intrinsic :: user_module\n  use, non_intrinsic :: iso_fortran_env\nend module\n",
        )
        .unwrap();

        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.uses, vec!["iso_fortran_env"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_finds_semicolon_separated_uses_outside_literals_and_comments() {
        let dir = std::env::temp_dir().join("dep_scan_semicolon_uses");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("consumer.f90");
        std::fs::write(
            &f,
            "module consumer; use first; character(len=*), parameter :: marker = 'don''t; use fake'; use second !; use ignored\nprint *, \"semi; ! use fake_too\"; use third\nend module\n",
        )
        .unwrap();

        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.defines, vec!["consumer"]);
        assert_eq!(deps.uses, vec!["first", "second", "third"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_ignores_semicolons_in_fixed_comments_and_hollerith_text() {
        let dir = std::env::temp_dir().join("dep_scan_fixed_semicolons");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("producer.f");
        std::fs::write(
            &f,
            "      MODULE A\nC COMMENT; USE COMMENT_DEP\n      CONTAINS\n      SUBROUTINE SHOW(I)\n  100 FORMAT(6H;USE B,I2)\n      END\n      END\n",
        )
        .unwrap();
        let config = crate::preprocess::PreprocConfig {
            fixed_form: true,
            ..Default::default()
        };

        let deps = scan_file(&f, &config).unwrap();
        assert_eq!(deps.defines, vec!["a"]);
        assert!(
            deps.uses.is_empty(),
            "unexpected dependencies: {:?}",
            deps.uses
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_ignores_prefixed_module_procedures() {
        let dir = std::env::temp_dir().join("dep_scan_prefixed_module_procedures");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("contracts.f90");
        std::fs::write(
            &f,
            "module contracts\ninterface\n  module pure function value()\n    integer :: value\n  end function value\nend interface\nend module contracts\n",
        )
        .unwrap();

        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.defines, vec!["contracts"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_ignores_inactive_preprocessor_branches() {
        let dir = std::env::temp_dir().join("dep_scan_inactive_branch");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.F90");
        std::fs::write(&f, "module a\n#if 0\n  use b\n#endif\nend module a\n").unwrap();

        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.defines, vec!["a"]);
        assert!(deps.uses.is_empty(), "inactive USE became a dependency");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_direct_submodule() {
        let dir = std::env::temp_dir().join("dep_scan_submod_1");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("impl.f90");
        std::fs::write(
            &f,
            "submodule (myparent) impl\ncontains\n  module procedure foo\n  end procedure\nend submodule\n",
        )
        .unwrap();
        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        // Depends on the parent module; `module procedure foo` is not a def.
        assert_eq!(deps.uses, vec!["myparent"]);
        assert_eq!(deps.defines, vec!["myparent:impl"]);
        assert_eq!(
            deps.submodule_of,
            Some(("myparent".to_string(), "myparent".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_nested_submodule() {
        let dir = std::env::temp_dir().join("dep_scan_submod_2");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("grand.f90");
        std::fs::write(&f, "submodule (anc:par) grand\nend submodule\n").unwrap();
        let deps = scan_file(&f, &crate::preprocess::PreprocConfig::default()).unwrap();
        assert_eq!(deps.uses, vec!["anc:par"]);
        assert_eq!(deps.defines, vec!["anc:grand"]);
        assert_eq!(
            deps.submodule_of,
            Some(("anc".to_string(), "anc:par".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topo_sort_submodule_after_parent() {
        // Worst input order: submodule, then its parent module, then a
        // consumer. The submodule must land after the parent.
        let files = vec![
            FileDeps {
                path: "child.f90".into(),
                defines: vec!["mp:impl".into()],
                uses: vec!["mp".into()],
                submodule_of: Some(("mp".into(), "mp".into())),
            },
            FileDeps {
                path: "parent.f90".into(),
                defines: vec!["mp".into()],
                uses: vec![],
                submodule_of: None,
            },
            FileDeps {
                path: "main.f90".into(),
                defines: vec![],
                uses: vec!["mp".into()],
                submodule_of: None,
            },
        ];
        let order = resolve_compilation_order(&files).unwrap();
        let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(1) < pos(0), "parent must precede child submodule");
        assert!(pos(1) < pos(2), "parent must precede consumer");
    }

    #[test]
    fn topo_sort_nested_submodule_chain() {
        // grand -> child -> module: nested submodule chain in worst order.
        let files = vec![
            FileDeps {
                path: "grand.f90".into(),
                defines: vec!["nm:grand".into()],
                uses: vec!["nm:child".into()],
                submodule_of: Some(("nm".into(), "nm:child".into())),
            },
            FileDeps {
                path: "child.f90".into(),
                defines: vec!["nm:child".into()],
                uses: vec!["nm".into()],
                submodule_of: Some(("nm".into(), "nm".into())),
            },
            FileDeps {
                path: "mod.f90".into(),
                defines: vec!["nm".into()],
                uses: vec![],
                submodule_of: None,
            },
        ];
        let order = resolve_compilation_order(&files).unwrap();
        let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(2) < pos(1), "module before child");
        assert!(pos(1) < pos(0), "child before grand");
    }

    #[test]
    fn topo_sort_chain() {
        let files = vec![
            FileDeps {
                path: "c.f90".into(),
                defines: vec!["c".into()],
                uses: vec![],
                submodule_of: None,
            },
            FileDeps {
                path: "b.f90".into(),
                defines: vec!["b".into()],
                uses: vec!["c".into()],
                submodule_of: None,
            },
            FileDeps {
                path: "a.f90".into(),
                defines: vec!["a".into()],
                uses: vec!["b".into()],
                submodule_of: None,
            },
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
            FileDeps {
                path: "a.f90".into(),
                defines: vec!["a".into()],
                uses: vec!["b".into()],
                submodule_of: None,
            },
            FileDeps {
                path: "b.f90".into(),
                defines: vec!["b".into()],
                uses: vec!["a".into()],
                submodule_of: None,
            },
        ];
        let err = resolve_compilation_order(&files).unwrap_err();
        assert!(
            err.contains("circular"),
            "expected cycle error, got: {}",
            err
        );
    }

    #[test]
    fn topo_sort_diamond() {
        let files = vec![
            FileDeps {
                path: "d.f90".into(),
                defines: vec!["d".into()],
                uses: vec![],
                submodule_of: None,
            },
            FileDeps {
                path: "b.f90".into(),
                defines: vec!["b".into()],
                uses: vec!["d".into()],
                submodule_of: None,
            },
            FileDeps {
                path: "c.f90".into(),
                defines: vec!["c".into()],
                uses: vec!["d".into()],
                submodule_of: None,
            },
            FileDeps {
                path: "a.f90".into(),
                defines: vec!["a".into()],
                uses: vec!["b".into(), "c".into()],
                submodule_of: None,
            },
        ];
        let order = resolve_compilation_order(&files).unwrap();
        let pos_d = order.iter().position(|&i| i == 0).unwrap();
        let pos_a = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_d < pos_a);
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn topo_sort_rejects_duplicate_module_definitions() {
        let files = vec![
            FileDeps {
                path: "first.f90".into(),
                defines: vec!["shared_name".into()],
                uses: vec![],
                submodule_of: None,
            },
            FileDeps {
                path: "second.f90".into(),
                defines: vec!["SHARED_NAME".into()],
                uses: vec![],
                submodule_of: None,
            },
        ];

        let err = resolve_compilation_order(&files).unwrap_err();
        assert!(err.contains("duplicate module definition 'shared_name'"));
        assert!(err.contains("first.f90"));
        assert!(err.contains("second.f90"));
    }
}
