//! l01: the F2023 million-character statement limit, exercised with a
//! generated source file (a checked-in ~1 MB fixture would make every
//! harness run at every opt level pay for it — sprint-doc pitfall).
//! Compile-only via `-S`, so this runs on every host. Acceptance never
//! changes: the over-limit statement still compiles, with the selected
//! standard deciding whether character count or continuation count is
//! the relevant conformance warning.

use std::path::{Path, PathBuf};
use std::process::Command;

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

/// One statement totalling just over a million characters, spread over
/// ~130-char continuation lines (under 132, so the F2018 run isolates
/// its continuation-count warning from the physical-line warning).
fn million_char_program() -> String {
    let mut src = String::with_capacity(1_100_000);
    src.push_str("program big_stmt\n  implicit none\n  integer :: total\n");
    src.push_str("  total = 0 &\n");
    let term = format!("    + {} &\n", "0".repeat(120));
    // The limit counts the statement's source characters; line content
    // is term minus the newline. Overshoot by a margin so the total is
    // unambiguously past one million.
    let lines_needed = 1_000_000 / (term.len() - 1) + 100;
    for line in 0..lines_needed {
        if line == lines_needed / 2 {
            src.push_str("! legal continuation gap\n\n");
        }
        src.push_str(&term);
    }
    src.push_str("    + 1\n  print *, total\nend program big_stmt\n");
    src
}

fn continued_program(continuations: usize) -> String {
    let mut src = String::from(
        "program continuation_limit\n  implicit none\n  integer :: total\n  total = 0",
    );
    for continuation in 0..continuations {
        src.push_str(" &\n");
        if continuation == continuations / 2 {
            src.push_str("! legal continuation gap\n\n");
        }
        src.push_str("    + 0");
    }
    src.push_str("\n  print *, total\nend program continuation_limit\n");
    src
}

fn continued_fixed_program(continuations: usize) -> String {
    let mut src =
        String::from("      PROGRAM CONTINUATION_LIMIT\n      INTEGER TOTAL\n      TOTAL = 0\n");
    for continuation in 0..continuations {
        if continuation == continuations / 2 {
            src.push_str("C legal continuation gap\n\n");
        }
        src.push_str("     + + 0\n");
    }
    src.push_str("      PRINT *, TOTAL\n      END\n");
    src
}

fn continued_character_program(leading_ampersand: bool) -> String {
    let continuation = if leading_ampersand {
        "    &world'"
    } else {
        "    world'"
    };
    format!(
        "program character_continuation\n\
         implicit none\n\
         character(len=*), parameter :: value = 'hello &\n\
         {continuation}\n\
         print *, value\n\
         end program character_continuation\n"
    )
}

fn macro_expanded_program(doublings: usize) -> String {
    let mut src = format!("#define A0 {}\n", "+1".repeat(2_048));
    for level in 1..=doublings {
        src.push_str(&format!("#define A{level} A{} A{}\n", level - 1, level - 1));
    }
    src.push_str(
        "program macro_limit\n\
         implicit none\n\
         integer :: total\n",
    );
    src.push_str(&format!("total = 0 A{doublings}\n"));
    src.push_str("end program macro_limit\n");
    src
}

fn compile_s(src_path: &Path, out: &Path, std_flag: &str) -> std::process::Output {
    Command::new(compiler())
        .args([std_flag, "-S"])
        .arg(src_path)
        .arg("-o")
        .arg(out)
        .output()
        .expect("cannot run armfortas")
}

#[cfg(target_os = "linux")]
#[test]
fn tiny_preprocess_fits_within_one_gibibyte_address_space() {
    let dir = std::env::temp_dir().join(format!("afs_stack_limit_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("line.F90");
    std::fs::write(&source, "program p\nend program\n").unwrap();

    let result = Command::new("sh")
        .args([
            "-c",
            "ulimit -v 1048576; exec \"$0\" -E \"$1\"",
            compiler().to_str().unwrap(),
            source.to_str().unwrap(),
        ])
        .output()
        .expect("cannot run constrained compiler");
    assert!(
        result.status.success(),
        "tiny preprocess exceeded a 1 GiB address space (status {:?}):\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn million_char_statement_uses_standard_specific_limit_warning() {
    let dir = std::env::temp_dir().join(format!("afs_srclim_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("big_stmt.f90");
    std::fs::write(&f90, million_char_program()).unwrap();
    let asm = dir.join("big_stmt.s");

    let r = compile_s(&f90, &asm, "--std=f2023");
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        r.status.success(),
        "over-limit statement must still compile (warning, never error):\n{}",
        stderr
    );
    assert!(
        stderr.contains("statement is") && stderr.contains("1,000,000"),
        "expected the F2023 statement-length conformance warning, got:\n{}",
        stderr
    );

    let r = compile_s(&f90, &asm, "--std=f2018");
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(r.status.success(), "f2018 compile failed:\n{}", stderr);
    assert!(
        !stderr.contains("warning: statement is"),
        "statement-length warning is F2023-only, got:\n{}",
        stderr
    );
    assert!(
        stderr.contains("statement has") && stderr.contains("continuation lines"),
        "the F2018 run must diagnose its continuation-count violation:\n{}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn f2018_continuation_limit_diagnoses_before_output_publication() {
    let dir = std::env::temp_dir().join(format!("afs_contlim_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conforming = dir.join("continuation_255.f90");
    let extension = dir.join("continuation_256.f90");
    let fixed_extension = dir.join("continuation_256.f");
    std::fs::write(&conforming, continued_program(255)).unwrap();
    std::fs::write(&extension, continued_program(256)).unwrap();
    std::fs::write(&fixed_extension, continued_fixed_program(256)).unwrap();

    let conforming_asm = dir.join("continuation_255.s");
    std::fs::write(&conforming_asm, b"stale conforming assembly").unwrap();
    let conforming_result = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S"])
        .arg(&conforming)
        .arg("-o")
        .arg(&conforming_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        conforming_result.status.success(),
        "255 continuation lines must remain conforming:\n{}",
        String::from_utf8_lossy(&conforming_result.stderr)
    );
    assert!(
        conforming_result.stderr.is_empty(),
        "the exact F2018 boundary emitted a diagnostic:\n{}",
        String::from_utf8_lossy(&conforming_result.stderr)
    );
    assert_ne!(
        std::fs::read(&conforming_asm).unwrap(),
        b"stale conforming assembly",
        "successful boundary compilation retained stale assembly"
    );

    let warning_asm = dir.join("continuation_warning.s");
    std::fs::write(&warning_asm, b"stale warning assembly").unwrap();
    let warning = Command::new(compiler())
        .args(["--std=f2018", "-O1", "-S"])
        .arg(&extension)
        .arg("-o")
        .arg(&warning_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        warning.status.success(),
        "continuation-count violation must remain a warning:\n{}",
        String::from_utf8_lossy(&warning.stderr)
    );
    let warning_stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(
        warning_stderr.contains("warning: statement has 256 continuation lines")
            && warning_stderr.contains("F2018 limit of 255"),
        "missing F2018 continuation-count warning:\n{warning_stderr}"
    );
    assert_ne!(
        std::fs::read(&warning_asm).unwrap(),
        b"stale warning assembly",
        "successful warning compilation retained stale assembly"
    );

    let fixed_warning_asm = dir.join("continuation_fixed_warning.s");
    let fixed_warning = Command::new(compiler())
        .args(["--std=f2018", "-O1", "-S"])
        .arg(&fixed_extension)
        .arg("-o")
        .arg(&fixed_warning_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        fixed_warning.status.success(),
        "fixed-form continuation-count violation must remain a warning:\n{}",
        String::from_utf8_lossy(&fixed_warning.stderr)
    );
    assert!(
        String::from_utf8_lossy(&fixed_warning.stderr)
            .contains("warning: statement has 256 continuation lines"),
        "fixed form lost the F2018 continuation-count warning:\n{}",
        String::from_utf8_lossy(&fixed_warning.stderr)
    );
    assert!(
        fixed_warning_asm.is_file(),
        "fixed-form warning compilation did not publish assembly"
    );

    for optimization in ["-O0", "-O3"] {
        for stale in [false, true] {
            let state = if stale { "stale" } else { "fresh" };
            let asm = dir.join(format!("continuation_{optimization}_{state}.s"));
            let depfile = asm.with_extension("d");
            if stale {
                std::fs::write(&asm, b"stale failed assembly").unwrap();
                std::fs::write(&depfile, b"stale failed dependencies").unwrap();
            }

            let failed = Command::new(compiler())
                .args(["--std=f2018", "-Werror", optimization, "-S", "-MD", "-MF"])
                .arg(&depfile)
                .arg(&extension)
                .arg("-o")
                .arg(&asm)
                .env("NO_COLOR", "1")
                .output()
                .expect("cannot run armfortas");
            assert_eq!(
                failed.status.code(),
                Some(1),
                "continuation -Werror must fail at {optimization} with {state} output:\n{}",
                String::from_utf8_lossy(&failed.stderr)
            );
            assert!(
                String::from_utf8_lossy(&failed.stderr)
                    .contains("error: statement has 256 continuation lines"),
                "continuation warning was not promoted at {optimization} with {state} output:\n{}",
                String::from_utf8_lossy(&failed.stderr)
            );
            assert!(
                !asm.exists(),
                "failed continuation -Werror retained {state} output at {optimization}"
            );
            assert!(
                !depfile.exists(),
                "failed continuation -Werror retained {state} dependency output at {optimization}"
            );
        }
    }

    let f2023_asm = dir.join("continuation_f2023.s");
    std::fs::write(&f2023_asm, b"stale f2023 assembly").unwrap();
    let f2023 = Command::new(compiler())
        .args(["--std=f2023", "-Werror", "-O2", "-S"])
        .arg(&extension)
        .arg("-o")
        .arg(&f2023_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        f2023.status.success(),
        "F2023 removed the continuation-count limit:\n{}",
        String::from_utf8_lossy(&f2023.stderr)
    );
    assert!(
        f2023.stderr.is_empty(),
        "F2023 emitted a continuation-count diagnostic:\n{}",
        String::from_utf8_lossy(&f2023.stderr)
    );
    assert_ne!(
        std::fs::read(&f2023_asm).unwrap(),
        b"stale f2023 assembly",
        "successful F2023 compilation retained stale assembly"
    );

    let fixed_f2023_asm = dir.join("continuation_fixed_f2023.s");
    std::fs::write(&fixed_f2023_asm, b"stale fixed f2023 assembly").unwrap();
    let fixed_f2023 = Command::new(compiler())
        .args(["--std=f2023", "-Werror", "-O2", "-S"])
        .arg(&fixed_extension)
        .arg("-o")
        .arg(&fixed_f2023_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        fixed_f2023.status.success(),
        "F2023 removed the fixed-form continuation-count limit:\n{}",
        String::from_utf8_lossy(&fixed_f2023.stderr)
    );
    assert!(
        fixed_f2023.stderr.is_empty(),
        "F2023 fixed form emitted a continuation-count diagnostic:\n{}",
        String::from_utf8_lossy(&fixed_f2023.stderr)
    );
    assert_ne!(
        std::fs::read(&fixed_f2023_asm).unwrap(),
        b"stale fixed f2023 assembly",
        "successful fixed-form F2023 compilation retained stale assembly"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strict_character_continuation_warns_and_werror_prevents_output_publication() {
    let dir = std::env::temp_dir().join(format!("afs_char_continuation_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let extension = dir.join("missing_ampersand.f90");
    let conforming = dir.join("leading_ampersand.f90");
    std::fs::write(&extension, continued_character_program(false)).unwrap();
    std::fs::write(&conforming, continued_character_program(true)).unwrap();

    let permissive_asm = dir.join("permissive.s");
    let permissive = Command::new(compiler())
        .arg("-S")
        .arg(&extension)
        .arg("-o")
        .arg(&permissive_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run default character-continuation case");
    assert!(
        permissive.status.success(),
        "default mode should accept the GNU-compatible extension:\n{}",
        String::from_utf8_lossy(&permissive.stderr)
    );
    assert!(
        permissive.stderr.is_empty(),
        "default mode diagnosed a strict conformance extension:\n{}",
        String::from_utf8_lossy(&permissive.stderr)
    );
    assert!(permissive_asm.is_file());

    let warning_asm = dir.join("warning.s");
    let warning = Command::new(compiler())
        .args(["--std=f2018", "-S"])
        .arg(&extension)
        .arg("-o")
        .arg(&warning_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run strict character-continuation warning case");
    assert!(
        warning.status.success(),
        "the GNU-compatible extension should remain compilable without -Werror:\n{}",
        String::from_utf8_lossy(&warning.stderr)
    );
    let warning_stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(
        warning_stderr.contains(":4:5: warning:")
            && warning_stderr.contains("missing '&' at the start of a continued character literal"),
        "missing strict character-continuation warning:\n{warning_stderr}"
    );
    assert!(
        warning_asm.is_file(),
        "non-promoted continuation warning did not publish assembly"
    );

    let failed_asm = dir.join("failed.s");
    std::fs::write(&failed_asm, b"stale assembly").unwrap();
    let failed = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S"])
        .arg(&extension)
        .arg("-o")
        .arg(&failed_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run promoted character-continuation warning case");
    assert!(
        !failed.status.success(),
        "-Werror did not promote the character-continuation warning"
    );
    let failed_stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        failed_stderr.contains(":4:5: error:")
            && failed_stderr.contains("missing '&' at the start of a continued character literal"),
        "promoted continuation diagnostic mismatch:\n{failed_stderr}"
    );
    assert!(
        !failed_asm.exists(),
        "promoted continuation diagnostic retained stale assembly"
    );

    let suppressed_asm = dir.join("suppressed.s");
    let suppressed = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-w", "-S"])
        .arg(&extension)
        .arg("-o")
        .arg(&suppressed_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run suppressed character-continuation warning case");
    assert!(
        suppressed.status.success(),
        "-w should suppress the promoted conformance warning:\n{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
    assert!(
        suppressed.stderr.is_empty(),
        "-w did not suppress the character-continuation diagnostic:\n{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
    assert!(suppressed_asm.is_file());

    let conforming_asm = dir.join("conforming.s");
    let conforming_result = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S"])
        .arg(&conforming)
        .arg("-o")
        .arg(&conforming_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run conforming character-continuation case");
    assert!(
        conforming_result.status.success(),
        "leading '&' must satisfy strict continuation syntax:\n{}",
        String::from_utf8_lossy(&conforming_result.stderr)
    );
    assert!(
        conforming_result.stderr.is_empty(),
        "conforming character continuation emitted a diagnostic:\n{}",
        String::from_utf8_lossy(&conforming_result.stderr)
    );
    assert!(conforming_asm.is_file());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn strict_character_continuation_warning_preserves_included_source_location() {
    let dir = std::env::temp_dir().join(format!("afs_char_cont_include_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("root.F90");
    let include = dir.join("continued.inc");
    let asm = dir.join("root.s");
    std::fs::write(&root, "#include \"continued.inc\"\n").unwrap();
    std::fs::write(&include, continued_character_program(false)).unwrap();

    let result = Command::new(compiler())
        .args(["--std=f2018", "-S"])
        .arg(&root)
        .arg("-o")
        .arg(&asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run included character-continuation case");
    assert!(
        result.status.success(),
        "included continuation extension should remain a warning:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains(&format!("{}:4:5: warning:", include.display())),
        "warning did not use the included physical location:\n{stderr}"
    );
    assert!(
        stderr.contains("4 |     world'"),
        "warning did not show the included continuation line:\n{stderr}"
    );
    assert!(asm.is_file());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn werror_promotes_source_limit_warnings_before_output_publication() {
    let dir = std::env::temp_dir().join(format!("afs_srclim_werror_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("overlong_comment.f90");
    let source = format!(
        "program p\n! {}\n  print *, 7\nend program p\n",
        "x".repeat(140)
    );
    std::fs::write(&f90, source).unwrap();

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        for stale in [false, true] {
            let state = if stale { "stale" } else { "fresh" };
            let asm = dir.join(format!(
                "overlong_comment_{}_{}.s",
                &optimization[2..],
                state
            ));
            let depfile = asm.with_extension("d");
            if stale {
                std::fs::write(&asm, b"stale assembly").unwrap();
                std::fs::write(&depfile, b"stale dependencies").unwrap();
            }

            let result = Command::new(compiler())
                .args(["--std=f2018", "-Werror", optimization, "-S", "-MD"])
                .arg("-MF")
                .arg(&depfile)
                .arg(&f90)
                .arg("-o")
                .arg(&asm)
                .env("NO_COLOR", "1")
                .output()
                .expect("cannot run armfortas");
            assert_eq!(
                result.status.code(),
                Some(1),
                "source-limit -Werror must fail at {optimization} with {state} output:\n{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                stderr.contains("error: line is 142 characters long")
                    && stderr.contains("limits free-form lines to 132"),
                "source-limit warning was not promoted at {optimization} with {state} output:\n{stderr}"
            );
            assert!(
                !asm.exists(),
                "failed source-limit -Werror retained {state} output at {optimization}"
            );
            assert!(
                !depfile.exists(),
                "failed source-limit -Werror retained {state} dependency output at {optimization}"
            );
        }
    }

    let warning_asm = dir.join("warning.s");
    let warning = Command::new(compiler())
        .args(["--std=f2018", "-S"])
        .arg(&f90)
        .arg("-o")
        .arg(&warning_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        warning.status.success(),
        "source-limit warning without -Werror must still compile:\n{}",
        String::from_utf8_lossy(&warning.stderr)
    );
    assert!(
        String::from_utf8_lossy(&warning.stderr).contains("warning: line is 142 characters long"),
        "non-promoted source-limit diagnostic lost warning severity:\n{}",
        String::from_utf8_lossy(&warning.stderr)
    );
    assert!(
        warning_asm.is_file(),
        "non-promoted source-limit warning did not publish assembly"
    );

    let overridden_asm = dir.join("overridden.s");
    let overridden = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-ffree-line-length-142", "-S"])
        .arg(&f90)
        .arg("-o")
        .arg(&overridden_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        overridden.status.success(),
        "a matching numeric line limit must prevent promotion:\n{}",
        String::from_utf8_lossy(&overridden.stderr)
    );
    assert!(
        overridden.stderr.is_empty(),
        "matching numeric line limit emitted a diagnostic:\n{}",
        String::from_utf8_lossy(&overridden.stderr)
    );
    assert!(
        overridden_asm.is_file(),
        "numeric line-limit override did not publish assembly"
    );

    let suppressed_asm = dir.join("suppressed.s");
    let suppressed = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-w", "-S"])
        .arg(&f90)
        .arg("-o")
        .arg(&suppressed_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        suppressed.status.success(),
        "-w must suppress source-limit promotion:\n{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
    assert!(
        suppressed.stderr.is_empty(),
        "-w leaked source-limit diagnostics:\n{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
    assert!(
        suppressed_asm.is_file(),
        "suppressed source-limit warning did not publish assembly"
    );

    let preprocess = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-E"])
        .arg(&f90)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(
        preprocess.status.code(),
        Some(1),
        "source-limit -Werror must fail before preprocess output"
    );
    assert!(
        preprocess.stdout.is_empty(),
        "failed source-limit -Werror published preprocess output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn source_limit_failure_cleanup_preserves_included_inputs() {
    let dir = std::env::temp_dir().join(format!("afs_srclim_aliases_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let include = dir.join("protected.inc");
    let include_contents = b"integer, parameter :: answer = 7\n";
    std::fs::write(&include, include_contents).unwrap();
    let including_source = dir.join("overlong_include.F90");
    std::fs::write(
        &including_source,
        format!(
            "program p\n#include \"protected.inc\"\n! {}\n  print *, answer\nend program p\n",
            "x".repeat(140)
        ),
    )
    .unwrap();
    let alias_asm = dir.join("include_alias.s");
    std::fs::write(&alias_asm, b"stale assembly").unwrap();
    let include_alias = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S", "-MD", "-MF"])
        .arg(&include)
        .arg(&including_source)
        .arg("-o")
        .arg(&alias_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(
        include_alias.status.code(),
        Some(1),
        "source-limit -Werror with an include/depfile alias must fail"
    );
    let include_alias_stderr = String::from_utf8_lossy(&include_alias.stderr);
    assert!(
        include_alias_stderr.contains("error: line is 142 characters long")
            && include_alias_stderr.contains("conflicts with compiler input"),
        "include/depfile alias did not retain both failure diagnostics:\n{include_alias_stderr}"
    );
    assert_eq!(
        std::fs::read(&include).expect("protected include disappeared"),
        include_contents,
        "source-limit cleanup mutated an included compiler input"
    );
    assert!(
        !alias_asm.exists(),
        "include/depfile conflict retained stale primary output"
    );

    let output_alias = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S"])
        .arg(&including_source)
        .arg("-o")
        .arg(&include)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(
        output_alias.status.code(),
        Some(1),
        "source-limit -Werror with an include/output alias must fail"
    );
    assert_eq!(
        std::fs::read(&include).expect("output-aliased include disappeared"),
        include_contents,
        "source-limit cleanup mutated an output-aliased compiler input"
    );

    #[cfg(unix)]
    {
        let include_symlink = dir.join("protected_alias.inc");
        std::os::unix::fs::symlink(&include, &include_symlink).unwrap();
        let symlink_source = dir.join("overlong_symlink_include.F90");
        std::fs::write(
            &symlink_source,
            format!(
                "program p\n#include \"protected_alias.inc\"\n! {}\n  print *, answer\nend program p\n",
                "x".repeat(140)
            ),
        )
        .unwrap();

        let symlink_depfile_asm = dir.join("symlink_depfile_alias.s");
        std::fs::write(&symlink_depfile_asm, b"stale assembly").unwrap();
        let symlink_depfile_alias = Command::new(compiler())
            .args(["--std=f2018", "-Werror", "-S", "-MD", "-MF"])
            .arg(&include)
            .arg(&symlink_source)
            .arg("-o")
            .arg(&symlink_depfile_asm)
            .env("NO_COLOR", "1")
            .output()
            .expect("cannot run armfortas");
        assert_eq!(
            symlink_depfile_alias.status.code(),
            Some(1),
            "source-limit cleanup accepted a depfile alias hidden by an include symlink"
        );
        assert!(
            String::from_utf8_lossy(&symlink_depfile_alias.stderr)
                .contains("conflicts with compiler input"),
            "missing symlink-hidden depfile conflict diagnostic:\n{}",
            String::from_utf8_lossy(&symlink_depfile_alias.stderr)
        );
        assert_eq!(
            std::fs::read(&include).expect("symlink-aliased include target disappeared"),
            include_contents,
            "source-limit cleanup mutated a symlink-aliased include target"
        );
        assert!(
            !symlink_depfile_asm.exists(),
            "symlink-hidden depfile conflict retained stale primary output"
        );

        let symlink_output_alias = Command::new(compiler())
            .args(["--std=f2018", "-Werror", "-S"])
            .arg(&symlink_source)
            .arg("-o")
            .arg(&include)
            .env("NO_COLOR", "1")
            .output()
            .expect("cannot run armfortas");
        assert_eq!(
            symlink_output_alias.status.code(),
            Some(1),
            "source-limit cleanup accepted an output alias hidden by an include symlink"
        );
        assert_eq!(
            std::fs::read(&include).expect("symlink-hidden output target disappeared"),
            include_contents,
            "source-limit cleanup mutated a symlink-hidden output target"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn source_limit_promotion_retains_preprocessor_error() {
    let dir = std::env::temp_dir().join(format!("afs_srclim_pp_error_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let missing_include_source = dir.join("overlong_missing_include.F90");
    std::fs::write(
        &missing_include_source,
        format!(
            "program p\n#include \"missing.inc\"\n! {}\nend program p\n",
            "x".repeat(140)
        ),
    )
    .unwrap();
    let preprocess_failure_asm = dir.join("preprocess_failure.s");
    let preprocess_failure = Command::new(compiler())
        .args(["--std=f2018", "-Werror", "-S"])
        .arg(&missing_include_source)
        .arg("-o")
        .arg(&preprocess_failure_asm)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(
        preprocess_failure.status.code(),
        Some(1),
        "preprocessor failure after source-limit promotion must fail"
    );
    let preprocess_failure_stderr = String::from_utf8_lossy(&preprocess_failure.stderr);
    assert!(
        preprocess_failure_stderr.contains("error: line is 142 characters long")
            && preprocess_failure_stderr.contains("preprocessing failed"),
        "preprocessor failure lost promoted or causal diagnostics:\n{preprocess_failure_stderr}"
    );
    assert!(
        !preprocess_failure_asm.exists(),
        "preprocessor failure published a primary output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The explosion boundary (l01 follow-up): a deep expression chain
/// inside the statement cap must compile, not stack-fault. Keep this
/// large enough to require the compile-thread stack path, but below
/// the maximal conforming depth; the maximal 600k-term variant is a
/// manual stress case and takes too long for CI.
#[test]
fn deep_chain_within_cap_compiles() {
    let n = 20_000;
    let mut src = String::with_capacity(4 * n);
    src.push_str("program p\nimplicit none\ninteger :: total\ntotal=0&\n");
    for _ in 0..n - 1 {
        src.push_str("+1&\n");
    }
    src.push_str("+1\nprint *, total\nend program p\n");
    let dir = std::env::temp_dir().join(format!("afs_deepchain_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("deep.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("deep.s"), "--std=f2023");
    assert!(
        r.status.success(),
        "a {}-term chain inside the cap must compile (status {:?}):\n{}",
        n,
        r.status,
        String::from_utf8_lossy(&r.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Past the cap: a clean diagnostic and exit 1 — never a stack fault.
#[test]
fn over_cap_statement_errors_cleanly() {
    let n = 300_000;
    let mut src = String::with_capacity(8 * n);
    src.push_str("program p\nimplicit none\ninteger :: total\ntotal=0&\n");
    for term in 0..n {
        if term == n / 2 {
            src.push_str("! legal continuation gap\n\n");
        }
        src.push_str("+1     &\n"); // fat lines: past 2M chars
    }
    src.push_str("+1\nprint *, total\nend program p\n");
    let dir = std::env::temp_dir().join(format!("afs_overcap_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("overcap.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("overcap.s"), "--std=f2023");
    assert_eq!(
        r.status.code(),
        Some(1),
        "over-cap statement must exit 1 (a None code means a signal — the stack fault this gate exists to prevent)"
    );
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        stderr.contains("compiler limit"),
        "expected the statement-cap diagnostic, got:\n{}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn macro_expansion_is_rechecked_against_statement_hard_cap() {
    let dir = std::env::temp_dir().join(format!("afs_macro_cap_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("macro_cap.f90");
    std::fs::write(&f90, macro_expanded_program(9)).unwrap();

    for stale in [false, true] {
        let state = if stale { "stale" } else { "fresh" };
        let output = dir.join(format!("macro_cap_{state}.i"));
        let depfile = dir.join(format!("macro_cap_{state}.d"));
        if stale {
            std::fs::write(&output, b"stale preprocessed output").unwrap();
            std::fs::write(&depfile, b"stale dependencies").unwrap();
        }

        let result = Command::new(compiler())
            .args(["-E", "-MD", "-MF"])
            .arg(&depfile)
            .arg(&f90)
            .arg("-o")
            .arg(&output)
            .env("NO_COLOR", "1")
            .output()
            .expect("cannot run armfortas");
        assert_eq!(
            result.status.code(),
            Some(1),
            "expanded over-cap statement must exit 1 with {state} outputs"
        );
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("macro_cap.f90:14:1")
                && stderr.contains("statement expands to")
                && stderr.contains("after preprocessing")
                && stderr.contains("compiler limit"),
            "missing source-mapped expanded-statement diagnostic with {state} outputs:\n{stderr}"
        );
        assert!(
            result.stdout.is_empty(),
            "expanded over-cap statement published stdout with {state} outputs"
        );
        assert!(
            !output.exists(),
            "expanded over-cap statement retained {state} preprocess output"
        );
        assert!(
            !depfile.exists(),
            "expanded over-cap statement retained {state} dependency output"
        );
    }

    let under_cap = dir.join("macro_under_cap.f90");
    let under_cap_output = dir.join("macro_under_cap.i");
    std::fs::write(&under_cap, macro_expanded_program(8)).unwrap();
    std::fs::write(&under_cap_output, b"stale under-cap output").unwrap();
    let under_cap_result = Command::new(compiler())
        .arg("-E")
        .arg(&under_cap)
        .arg("-o")
        .arg(&under_cap_output)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert!(
        under_cap_result.status.success(),
        "expanded statement within the compiler cap was rejected:\n{}",
        String::from_utf8_lossy(&under_cap_result.stderr)
    );
    assert_ne!(
        std::fs::read(&under_cap_output).unwrap(),
        b"stale under-cap output",
        "successful under-cap preprocessing retained stale output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Pathological paren nesting stops at the parser's nesting limit with
/// a clean error (pre-existing guard, locked here alongside the cap).
#[test]
fn deep_paren_nesting_errors_cleanly() {
    let depth = 5_000;
    let src = format!(
        "program p\nimplicit none\ninteger :: x\nx = {}1{}\nprint *, x\nend program p\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let dir = std::env::temp_dir().join(format!("afs_deepparen_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("paren.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("paren.s"), "--std=f2023");
    assert_eq!(
        r.status.code(),
        Some(1),
        "deep nesting must exit 1, not fault"
    );
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("nesting exceeds parser limit"),
        "expected the parser nesting diagnostic"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
