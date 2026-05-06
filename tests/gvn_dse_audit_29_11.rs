use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn capture_text(request: CaptureRequest, stage: Stage) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(stage) {
        Some(CapturedStage::Text(text)) => text.clone(),
        Some(CapturedStage::Run(_)) => panic!("expected text stage for {}", stage.as_str()),
        None => panic!("missing requested stage {}", stage.as_str()),
    }
}

fn function_section<'a>(ir: &'a str, name: &str) -> &'a str {
    let header = format!("func @{}", name);
    let start = ir
        .find(&header)
        .unwrap_or_else(|| panic!("missing function section for {}", name));
    let rest = &ir[start..];
    let end = rest
        .find("\n  }\n")
        .unwrap_or_else(|| panic!("unterminated function section for {}", name));
    &rest[..end + "\n  }".len()]
}

fn function_sections(ir: &str) -> Vec<&str> {
    ir.match_indices("func @")
        .map(|(idx, _)| {
            let rest = &ir[idx..];
            let end = rest
                .find("\n  }\n")
                .unwrap_or_else(|| panic!("unterminated function section in:\n{}", rest));
            &rest[..end + "\n  }".len()]
        })
        .collect()
}

fn function_name<'a>(func_section: &'a str) -> &'a str {
    let header = func_section.lines().next().expect("function header").trim();
    let rest = header
        .strip_prefix("func @")
        .expect("function header prefix");
    let end = rest
        .find(|ch: char| ch == ' ' || ch == '(')
        .unwrap_or(rest.len());
    &rest[..end]
}

fn block_section<'a>(func_section: &'a str, prefix: &str) -> &'a str {
    let mut start = None;
    let mut end = None;

    for (idx, _line) in func_section.match_indices('\n') {
        let line_start = idx + 1;
        let tail = &func_section[line_start..];
        let line_text = tail.split_once('\n').map(|(line, _)| line).unwrap_or(tail);

        if start.is_none() {
            if line_text.starts_with("    ")
                && !line_text.starts_with("      ")
                && line_text[4..].starts_with(prefix)
            {
                start = Some(line_start);
            }
            continue;
        }

        if line_text.starts_with("    ") && !line_text.starts_with("      ") {
            end = Some(idx);
            break;
        }
        if line_text == "  }" {
            end = Some(idx);
            break;
        }
    }

    let start = start.unwrap_or_else(|| panic!("missing block with prefix {}", prefix));
    let end = end.unwrap_or(func_section.len());
    &func_section[start..end]
}

fn last_block_section<'a>(func_section: &'a str, prefix: &str) -> &'a str {
    let mut starts = Vec::new();

    for (idx, _line) in func_section.match_indices('\n') {
        let line_start = idx + 1;
        let tail = &func_section[line_start..];
        let line_text = tail.split_once('\n').map(|(line, _)| line).unwrap_or(tail);
        if line_text.starts_with("    ")
            && !line_text.starts_with("      ")
            && line_text[4..].starts_with(prefix)
        {
            starts.push(line_start);
        }
    }

    let start = *starts
        .last()
        .unwrap_or_else(|| panic!("missing block with prefix {}", prefix));
    let tail = &func_section[start..];
    let end_rel = tail
        .match_indices('\n')
        .find_map(|(idx, _)| {
            let line_start = idx + 1;
            let line_tail = &tail[line_start..];
            let line_text = line_tail
                .split_once('\n')
                .map(|(line, _)| line)
                .unwrap_or(line_tail);
            if (line_text.starts_with("    ") && !line_text.starts_with("      "))
                || line_text == "  }"
            {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(tail.len());
    &tail[..end_rel]
}

#[test]
fn o2_reuses_branch_join_affine_expression() {
    let source = fixture("realworld_join_bias_sum.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        3,
        "raw IR should include the program body plus a pure helper and a tally worker:\n{}",
        raw_ir
    );
    let helper_name = function_name(raw_sections[1]);
    let worker_name = function_name(raw_sections[2]);

    let raw_tally = function_section(&raw_ir, worker_name);
    let raw_join = last_block_section(raw_tally, "if_end_");

    assert!(
        raw_join.matches(&format!("call @{}", helper_name)).count() >= 2,
        "raw join block should still recompute the repeated branch-join PURE helper call:\n{}",
        raw_join
    );
    assert!(
        opt_ir.matches(&format!("call @{}", helper_name)).count()
            < raw_ir.matches(&format!("call @{}", helper_name)).count(),
        "O2 should reduce duplicated branch-join PURE helper calls:\n{}",
        opt_ir
    );
}

#[test]
fn o2_removes_dead_seed_store_across_noalias_call() {
    let source = fixture("realworld_seed_overwrite.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        3,
        "raw IR should include the program body plus a helper and a fill worker:\n{}",
        raw_ir
    );
    let worker_name = function_name(raw_sections[2]);

    let raw_fill = function_section(&raw_ir, worker_name);
    let opt_fill = function_section(&opt_ir, worker_name);
    let raw_body = block_section(raw_fill, "do_body_");
    let opt_body = block_section(opt_fill, "do_body_");

    assert!(
        raw_body.matches("store ").count() >= 2,
        "raw loop body should still contain the seed store and the real fill store:\n{}",
        raw_body
    );
    // Detect "seed-zero" pattern: a store whose value is a `const_int 0`.
    // This invariant is robust to loop transformations like partial
    // unrolling that multiply store counts — DSE removing the dead
    // seed eliminates the const-zero feeding any store.
    fn count_store_of_zero(func_section: &str) -> usize {
        use std::collections::HashSet;
        // Collect every value id defined as `const_int 0`.
        let zeros: HashSet<&str> = func_section
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                let after_eq = t.strip_prefix("%")?;
                let (id, rest) = after_eq.split_once(" = const_int 0 ")?;
                let _ = rest;
                Some(id)
            })
            .collect();
        // Count `store %ID, ...` where %ID is one of those.
        func_section
            .lines()
            .filter_map(|l| l.trim().strip_prefix("store %"))
            .filter(|rest| {
                let id = rest.split(',').next().unwrap_or("").trim();
                zeros.contains(id)
            })
            .count()
    }
    let raw_zeros = count_store_of_zero(raw_fill);
    let opt_zeros = count_store_of_zero(opt_fill);
    assert!(
        raw_zeros >= 1,
        "raw seed_and_fill should have at least one store-of-zero (the dead seed):\n{}",
        raw_fill
    );
    assert_eq!(
        opt_zeros, 0,
        "O2 DSE should remove the dead store-of-zero (raw had {}):\n{}",
        raw_zeros, opt_fill
    );
}
