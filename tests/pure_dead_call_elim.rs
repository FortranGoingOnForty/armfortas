use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

struct TempSource(PathBuf);

impl TempSource {
    fn new(stem: &str, source: &str) -> Self {
        let path = std::env::temp_dir().join(format!("afs_{stem}_{}.f90", std::process::id()));
        fs::write(&path, source).expect("temporary Fortran source should be writable");
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

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

fn capture_run_stdout(request: CaptureRequest) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run stage");
    assert_eq!(
        run.exit_code, 0,
        "program should exit successfully: {run:#?}"
    );
    run.stdout_text()
        .expect("run stdout should be valid UTF-8")
        .to_owned()
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

#[test]
fn o1_eliminates_unused_pure_recursive_call_from_program_entry() {
    let source = fixture("pure_dead_call.f90");

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
            opt_level: OptLevel::O1,
        },
        Stage::OptIr,
    );
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        2,
        "raw IR should include the program body plus one contained pure helper:\n{}",
        raw_ir
    );
    let helper_name = function_name(raw_sections[1]);

    let raw_main = function_section(&raw_ir, "__prog_pure_dead_call");
    let opt_main = function_section(&opt_ir, "__prog_pure_dead_call");

    assert!(
        raw_main.contains(&format!("call @{}(", helper_name)),
        "lowered caller should still contain the unused PURE call before optimization:\n{}",
        raw_main
    );
    assert!(
        !opt_main.contains(&format!("call @{}(", helper_name)),
        "O1 optimized caller should delete the dead PURE call:\n{}",
        opt_main
    );
}

#[test]
fn unused_impure_call_keeps_its_effect_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        armfortas::testing::report_harness_skip(
            "pure_dead_call_elim",
            "unused_impure_call_keeps_its_effect_across_all_opt_levels",
            1,
            &reason,
        );
        return;
    }

    let source = TempSource::new(
        "impure_dead_call",
        "program impure_dead_call\n\
           implicit none\n\
           integer :: sink\n\
           sink = noisy()\n\
           sink = 0\n\
           print *, 'done'\n\
         contains\n\
           integer function noisy()\n\
             print *, 'effect'\n\
             noisy = 7\n\
           end function noisy\n\
         end program impure_dead_call\n",
    );
    for opt_level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let stdout = capture_run_stdout(CaptureRequest {
            input: source.path(),
            requested: BTreeSet::from([Stage::Run]),
            opt_level,
        });
        let lines = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            ["effect", "done"],
            "discarding the result must not discard the call at {opt_level:?}:\n{stdout}"
        );
    }
}
