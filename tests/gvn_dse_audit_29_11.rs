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

fn block_section<'a>(func_section: &'a str, prefix: &str) -> &'a str {
    let mut start = None;
    let mut end = None;

    for (idx, _line) in func_section.match_indices('\n') {
        let line_start = idx + 1;
        let tail = &func_section[line_start..];
        let line_text = tail
            .split_once('\n')
            .map(|(line, _)| line)
            .unwrap_or(tail);

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
        let line_text = tail
            .split_once('\n')
            .map(|(line, _)| line)
            .unwrap_or(tail);
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

    let raw_tally = function_section(&raw_ir, "tally");
    let raw_join = last_block_section(raw_tally, "if_end_");

    assert!(
        raw_join.matches("call @offset_value").count() >= 2,
        "raw join block should still recompute the repeated branch-join PURE helper call:\n{}",
        raw_join
    );
    assert!(
        opt_ir.matches("call @offset_value").count() < raw_ir.matches("call @offset_value").count(),
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

    let raw_fill = function_section(&raw_ir, "seed_and_fill");
    let opt_fill = function_section(&opt_ir, "seed_and_fill");
    let raw_body = block_section(raw_fill, "do_body_");
    let opt_body = block_section(opt_fill, "do_body_");

    assert!(
        raw_body.matches("store ").count() >= 2,
        "raw loop body should still contain the seed store and the real fill store:\n{}",
        raw_body
    );
    assert!(
        opt_body.matches("store ").count() < raw_body.matches("store ").count(),
        "O2 should remove the dead seed store while keeping the real fill:\n{}",
        opt_body
    );
}
