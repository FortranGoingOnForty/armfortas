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
    let header = format!("  func @{}", name);
    let start = ir.find(&header).unwrap_or_else(|| panic!("missing function section for {}", name));
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

fn tail_after<'a>(text: &'a str, needle: &str) -> &'a str {
    let start = text.find(needle).unwrap_or_else(|| panic!("missing '{}' in:\n{}", needle, text));
    &text[start + needle.len()..]
}

#[test]
fn o2_hoists_affine_dummy_loads_out_of_loop() {
    let source = fixture("realworld_affine_shift.f90");

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

    let raw_apply = function_section(&raw_ir, "apply");
    let opt_apply = function_section(&opt_ir, "apply");
    let raw_body = block_section(raw_apply, "do_body_");
    let opt_preheader = block_section(opt_apply, "if_end_");
    let opt_body = block_section(opt_apply, "do_body_");

    assert!(
        raw_body.contains("load %5 : ptr<i32>") && raw_body.contains("load %7 : ptr<i32>"),
        "raw LICM kernel should still chase scalar dummy wrappers inside the loop before hoisting:\n{}",
        raw_body
    );
    assert!(
        opt_preheader.contains("load %0 : i32") && opt_preheader.contains("load %1 : i32"),
        "O2 LICM should hoist invariant dummy loads into the loop preheader:\n{}",
        opt_apply
    );
    assert!(
        !opt_body.contains("load %0 : i32") && !opt_body.contains("load %1 : i32"),
        "O2 loop body should reuse the hoisted dummy loads:\n{}",
        opt_apply
    );
}

#[test]
fn o2_forwards_local_store_reuse_across_noalias_call() {
    let source = fixture("realworld_noalias_reuse.f90");

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

    let raw_local = function_section(&raw_ir, "classify_local");
    let opt_local = function_section(&opt_ir, "classify_local");
    let raw_body = block_section(raw_local, "do_body_");
    let opt_body = block_section(opt_local, "do_body_");

    let raw_after_call = tail_after(raw_body, "call @touch");
    let opt_after_call = tail_after(opt_body, "call @touch");

    assert!(
        raw_after_call.contains("gep") && raw_after_call.contains("load"),
        "raw local LSF kernel should still recompute and reload y(i) after the helper call:\n{}",
        raw_body
    );
    assert!(
        !opt_after_call.contains("gep"),
        "O2 local LSF should reuse the stored y(i) value directly after the noalias helper call:\n{}",
        opt_body
    );
}

#[test]
fn o2_forwards_branch_join_reuse_across_noalias_side_call() {
    let source = fixture("realworld_noalias_reuse.f90");

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

    let raw_branchy = function_section(&raw_ir, "classify_branchy");
    let opt_branchy = function_section(&opt_ir, "classify_branchy");
    let raw_join = block_section(raw_branchy, "if_end_9");
    let opt_join = block_section(opt_branchy, "if_end_9");

    assert!(
        raw_join.contains("gep") && raw_join.contains("load"),
        "raw branch-join kernel should still reload y(i) after the side-path helper call:\n{}",
        raw_join
    );
    assert!(
        !opt_join.contains("gep"),
        "O2 global LSF should remove the join-block y(i) reload across the noalias side-path call:\n{}",
        opt_join
    );
}
