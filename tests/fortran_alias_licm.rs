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

#[test]
fn o2_hoists_noalias_dummy_load_into_loop_preheader() {
    let source = fixture("licm_noalias_dummy_load.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    let kernel = function_section(&opt_ir, "kernel");
    let preheader = block_section(kernel, "if_end_");
    let loop_body = block_section(kernel, "do_body_");

    assert!(
        preheader.contains("load %0"),
        "O2 kernel preheader should preload the non-aliasing dummy arg:\n{}",
        kernel
    );
    assert!(
        !loop_body.contains("load %0"),
        "O2 kernel loop body should reuse the hoisted dummy-arg load:\n{}",
        kernel
    );
}
