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
    ir.match_indices("  func @")
        .map(|(idx, _)| {
            let rest = &ir[idx..];
            let end = rest
                .find("\n  }\n")
                .unwrap_or_else(|| panic!("unterminated function section in:\n{}", rest));
            &rest[..end + "\n  }".len()]
        })
        .collect()
}

fn function_name(func_section: &str) -> &str {
    let header = func_section.lines().next().expect("function header").trim();
    let rest = header
        .strip_prefix("func @")
        .expect("function header prefix");
    let end = rest.find([' ', '(']).unwrap_or(rest.len());
    &rest[..end]
}

fn non_program_function_names(ir: &str) -> Vec<&str> {
    function_sections(ir)
        .into_iter()
        .map(function_name)
        .filter(|name| !name.starts_with("__prog_"))
        .collect()
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
    let end = tail
        .match_indices('\n')
        .find_map(|(idx, _)| {
            let line_start = idx + 1;
            let rest = &tail[line_start..];
            let line_text = rest.split_once('\n').map(|(line, _)| line).unwrap_or(rest);
            ((line_text.starts_with("    ") && !line_text.starts_with("      "))
                || line_text == "  }")
                .then_some(idx)
        })
        .unwrap_or(tail.len());
    &tail[..end]
}

fn tail_after<'a>(text: &'a str, needle: &str) -> &'a str {
    let start = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing '{}' in:\n{}", needle, text));
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
    let helper_names = non_program_function_names(&raw_ir);
    assert_eq!(
        helper_names.len(),
        1,
        "raw IR should include exactly one contained affine helper:\n{}",
        raw_ir
    );

    let raw_apply = function_section(&raw_ir, helper_names[0]);
    let opt_apply = function_section(&opt_ir, helper_names[0]);
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
    let helper_names = non_program_function_names(&raw_ir);
    assert_eq!(
        helper_names.len(),
        3,
        "raw IR should include a side-effect helper plus two contained noalias workers:\n{}",
        raw_ir
    );

    let raw_local = function_section(&raw_ir, helper_names[1]);
    let opt_local = function_section(&opt_ir, helper_names[1]);
    let raw_body = block_section(raw_local, "do_body_");
    let opt_body = block_section(opt_local, "do_body_");

    let side_effect_call = format!("call @{}", helper_names[0]);
    let raw_after_call = tail_after(raw_body, &side_effect_call);
    let opt_after_call = tail_after(opt_body, &side_effect_call);

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
    let helper_names = non_program_function_names(&raw_ir);
    assert_eq!(
        helper_names.len(),
        3,
        "raw IR should include a side-effect helper plus two contained noalias workers:\n{}",
        raw_ir
    );

    let raw_branchy = function_section(&raw_ir, helper_names[2]);
    let opt_branchy = function_section(&opt_ir, helper_names[2]);
    let raw_join = last_block_section(raw_branchy, "if_end_");
    let opt_join = last_block_section(opt_branchy, "if_end_");

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
