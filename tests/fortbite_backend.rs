use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
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

fn suspicious_cross_class_moves(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("movreg x") && trimmed.contains(", d")
                || trimmed.starts_with("movreg d") && trimmed.contains(", x")
                || trimmed.starts_with("movreg w") && trimmed.contains(", s")
                || trimmed.starts_with("movreg s") && trimmed.contains(", w")
        })
        .collect()
}

fn move_contexts(text: &str, needles: &[&str]) -> Vec<String> {
    let lines: Vec<_> = text.lines().collect();
    let mut contexts = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if needles.iter().any(|needle| trimmed.contains(needle)) {
            let func_start = (0..=idx)
                .rev()
                .find(|cursor| {
                    let candidate = lines[*cursor].trim();
                    candidate.starts_with("function ") || candidate.ends_with(':')
                })
                .unwrap_or(idx);
            let start = idx.saturating_sub(2);
            let end = (idx + 3).min(lines.len());
            contexts.push(format!(
                "{}\n{}",
                lines[func_start],
                lines[start..end].join("\n")
            ));
        }
    }
    contexts
}

#[test]
fn fortbite_complex_scalar_division_keeps_pointer_arithmetic_out_of_fp_regs() {
    let regalloc = capture_text(
        CaptureRequest {
            input: fixture("fortbite_complex_scalar_division.f90"),
            requested: [Stage::Regalloc].into_iter().collect(),
            opt_level: OptLevel::O0,
        },
        Stage::Regalloc,
    );
    let asm = capture_text(
        CaptureRequest {
            input: fixture("fortbite_complex_scalar_division.f90"),
            requested: [Stage::Asm].into_iter().collect(),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );
    let regalloc_bad = suspicious_cross_class_moves(&regalloc);
    let asm_bad = suspicious_cross_class_moves(&asm);
    let regalloc_contexts =
        move_contexts(&regalloc, &["movreg x", "movreg d", "movreg w", "movreg s"]);
    let asm_contexts = move_contexts(&asm, &["mov x", "mov d", "mov w", "mov s"]);

    assert!(
        regalloc_bad.is_empty() && asm_bad.is_empty(),
        "suspicious cross-class moves remained in backend capture\nregalloc:\n{}\nregalloc-contexts:\n{}\nasm:\n{}\nasm-contexts:\n{}",
        regalloc_bad.join("\n"),
        regalloc_contexts.join("\n---\n"),
        asm_bad.join("\n"),
        asm_contexts.join("\n---\n"),
    );
}
