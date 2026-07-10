//! Sprint x05 end-state gate: the curated program set compiles to
//! ELF64/EM_X86_64 relocatables via `--target x86_64-* -c`, the system
//! assembler accepts every emitted `.s`, and the load-bearing
//! instruction patterns (idiv choreography, setcc+movzx pairing, the
//! ucomi condition table, the stack-probe loop) are present in real
//! output — not just in unit tests. x86_64 ELF hosts only; skips with
//! a count elsewhere (x01 convention).

use std::path::{Path, PathBuf};
use std::process::Command;

use armfortas::target::{Arch, ObjectFormat, TargetSpec};

const PROGRAMS: &[&str] = &[
    "x05_int_loops",
    "x05_fp_compare",
    "x05_if_chains",
    "x05_mod_div",
    "x05_conversions",
    "x05_big_frame",
];

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn programs_dir() -> PathBuf {
    for dir in ["test_programs", "../test_programs"] {
        if Path::new(dir).exists() {
            return PathBuf::from(dir);
        }
    }
    panic!("cannot find test_programs/");
}

fn host_is_x86_elf() -> bool {
    let host = TargetSpec::host();
    host.arch == Arch::X86_64 && host.object_format() == ObjectFormat::Elf
}

fn skip(test: &str, count: usize) -> bool {
    if host_is_x86_elf() {
        return false;
    }
    eprintln!(
        "\nHARNESS_SKIP suite=x86_object_smoke test={} count={} reason=\"needs an x86_64 ELF host with system as\"",
        test, count
    );
    true
}

fn emit_asm(target: &str, program: &str) -> String {
    let src = programs_dir().join(format!("{}.f90", program));
    let out = std::env::temp_dir().join(format!(
        "afs_x86smoke_{}_{}_{}.s",
        program,
        target,
        std::process::id()
    ));
    let r = Command::new(compiler())
        .args(["--target", target, "-S"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("cannot run armfortas");
    assert!(
        r.status.success(),
        "{} failed to emit for {}:\n{}",
        program,
        target,
        String::from_utf8_lossy(&r.stderr)
    );
    let text = std::fs::read_to_string(&out).expect("cannot read emitted asm");
    let _ = std::fs::remove_file(&out);
    text
}

#[test]
fn curated_programs_compile_to_elf_objects() {
    if skip(
        "curated_programs_compile_to_elf_objects",
        PROGRAMS.len() * 2,
    ) {
        return;
    }
    let readelf =
        armfortas::testing::find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]);
    for target in ["x86_64-freebsd", "x86_64-linux-gnu"] {
        for program in PROGRAMS {
            let src = programs_dir().join(format!("{}.f90", program));
            let obj = std::env::temp_dir().join(format!(
                "afs_x86smoke_{}_{}_{}.o",
                program,
                target,
                std::process::id()
            ));
            let r = Command::new(compiler())
                .args(["--target", target, "-c"])
                .arg(&src)
                .arg("-o")
                .arg(&obj)
                .output()
                .expect("cannot run armfortas");
            assert!(
                r.status.success(),
                "{} -c failed for {}:\n{}",
                program,
                target,
                String::from_utf8_lossy(&r.stderr)
            );
            let hdr = Command::new(&readelf)
                .arg("-h")
                .arg(&obj)
                .output()
                .expect("cannot run readelf");
            let hdr_text = String::from_utf8_lossy(&hdr.stdout);
            assert!(
                hdr_text.contains("X86-64") && hdr_text.contains("ELF64"),
                "{} ({}) is not an ELF64/EM_X86_64 object:\n{}",
                program,
                target,
                hdr_text
            );
            let syms = Command::new(&readelf)
                .args(["-s", "-W"])
                .arg(&obj)
                .output()
                .expect("cannot run readelf -s");
            let syms_text = String::from_utf8_lossy(&syms.stdout);
            assert!(
                syms_text.contains(&format!("__prog_{}", program)) && syms_text.contains("main"),
                "{} ({}) lacks expected symbols:\n{}",
                program,
                target,
                syms_text
            );
            let _ = std::fs::remove_file(&obj);
        }
    }
}

#[test]
fn division_uses_fixed_register_choreography() {
    if skip("division_uses_fixed_register_choreography", 1) {
        return;
    }
    let asm = emit_asm("x86_64-freebsd", "x05_mod_div");
    assert!(asm.contains("cltd"), "missing cltd before idivl:\n{}", asm);
    assert!(asm.contains("idivl"), "missing idivl:\n{}", asm);
}

#[test]
fn fp_compares_use_ucomi_with_unsigned_conditions() {
    if skip("fp_compares_use_ucomi_with_unsigned_conditions", 1) {
        return;
    }
    let asm = emit_asm("x86_64-freebsd", "x05_fp_compare");
    assert!(asm.contains("ucomisd"), "missing ucomisd:\n{}", asm);
    // The condition table forbids setb/jb (CF-reading, NaN-true);
    // every materialized FP relation goes through a/ae.
    for line in asm.lines() {
        let t = line.trim();
        assert!(
            !t.starts_with("setb ") && !t.starts_with("jb "),
            "CF-reading FP condition leaked (NaN-true): {}",
            line
        );
    }
    assert!(
        asm.contains("seta") || asm.contains("setae") || asm.contains("ja ") || asm.contains("jae"),
        "no unsigned-condition consumer found:\n{}",
        asm
    );
}

#[test]
fn setcc_is_always_paired_with_zero_extension() {
    if skip("setcc_is_always_paired_with_zero_extension", 1) {
        return;
    }
    let asm = emit_asm("x86_64-freebsd", "x05_if_chains");
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let mut found = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("set") {
            found += 1;
            // The next few instructions must zero-extend or store the
            // byte; a bare setcc whose 64-bit consumer reads stale high
            // bits is the bug this pins.
            let window = &lines[i + 1..(i + 4).min(lines.len())];
            assert!(
                window
                    .iter()
                    .any(|l| l.starts_with("movz") || l.starts_with("movb")),
                "setcc without movzx/byte-store nearby: {} | window {:?}",
                line,
                window
            );
        }
    }
    assert!(found > 0, "expected materialized booleans in:\n{}", asm);
}

#[test]
fn big_frames_emit_the_probe_loop() {
    if skip("big_frames_emit_the_probe_loop", 1) {
        return;
    }
    let asm = emit_asm("x86_64-freebsd", "x05_big_frame");
    let touches = asm.matches("orq $0, (%rsp)").count();
    assert!(
        touches >= 10,
        "48KB frame should probe ~12 pages, saw {}:\n{}",
        touches,
        asm
    );
    // Sub-then-touch order: the first probe touch must come after a
    // page-sized sub, never after the full-frame sub.
    assert!(
        !asm.contains(&format!("subq ${}, %rsp", 48 * 1024)),
        "single big sub defeats probing:\n{}",
        asm
    );
}

/// Conversion instructions carry the GP width in their suffix; the
/// XMM side has its own. The naive allocator once sized FP slot
/// traffic off the suffix: `cvtsi2sdl` stored its double def with
/// movss (4 of 8 bytes) and `cvttsd2sil` loaded its double source
/// with movss — silent wrong answers at runtime (x06). Pin the
/// allocator's load/store widths around both.
#[test]
fn conversion_spill_traffic_uses_fp_width() {
    if skip("conversion_spill_traffic_uses_fp_width", 1) {
        return;
    }
    let asm = emit_asm("x86_64-freebsd", "x05_conversions");
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    for (i, line) in lines.iter().enumerate() {
        let operands: Vec<&str> = line
            .split_once(' ')
            .map(|(_, ops)| ops.split(',').map(str::trim).collect())
            .unwrap_or_default();
        if line.starts_with("cvtsi2sd") {
            let def = operands
                .get(1)
                .expect("cvtsi2sd destination operand")
                .trim_end_matches(',');
            let narrow_store = lines
                .iter()
                .skip(i + 1)
                .take(4)
                .any(|next| next.starts_with("movss") && next.split(',').next() == Some(def));
            assert!(
                !narrow_store,
                "cvtsi2sd double def stored narrow near: {} | {}",
                line,
                lines
                    .iter()
                    .skip(i + 1)
                    .take(4)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        if line.starts_with("cvttsd2si") {
            let src = operands
                .first()
                .expect("cvttsd2si source operand")
                .trim_end_matches(',');
            let narrow_load = lines.iter().take(i).rev().take(4).any(|prev| {
                prev.starts_with("movss")
                    && prev
                        .split(',')
                        .nth(1)
                        .map(str::trim)
                        .map(|dst| dst.trim_end_matches(','))
                        == Some(src)
            });
            assert!(
                !narrow_load,
                "cvttsd2si double source loaded narrow near: {} | {}",
                lines
                    .iter()
                    .take(i)
                    .rev()
                    .take(4)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" | "),
                line,
            );
        }
    }
    assert!(
        lines.iter().any(|l| l.starts_with("cvtsi2sd"))
            && lines.iter().any(|l| l.starts_with("cvttsd2si")),
        "expected both conversion directions in:\n{}",
        asm
    );
}

/// The anti-32KB-bug gate at 1MB: many sub-threshold arrays force a
/// ~1MB stack frame; the same frame code must handle it (no
/// size-dependent encoding paths), and every page gets probed.
#[test]
fn one_megabyte_frame_compiles_and_probes() {
    if skip("one_megabyte_frame_compiles_and_probes", 1) {
        return;
    }
    let mut src = String::from(
        "! Generated by x86_object_smoke: ~1MB of locals via 20 arrays\n\
         ! each under the 64KB heap threshold.\nprogram big1m\n  implicit none\n",
    );
    for i in 0..20 {
        src.push_str(&format!("  integer :: a{}(13000)\n", i));
    }
    src.push_str("  integer :: i\n");
    for i in 0..20 {
        src.push_str(&format!(
            "  do i = 1, 13000\n    a{}(i) = i + {}\n  end do\n",
            i, i
        ));
    }
    src.push_str("  print *, a0(1) + a19(13000)\nend program big1m\n");
    let dir = std::env::temp_dir().join(format!("afs_x86_1mb_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let f90 = dir.join("big1m.f90");
    let s_path = dir.join("big1m.s");
    std::fs::write(&f90, src).unwrap();
    let r = Command::new(compiler())
        .args(["--target", "x86_64-freebsd", "-S"])
        .arg(&f90)
        .arg("-o")
        .arg(&s_path)
        .output()
        .expect("cannot run armfortas");
    assert!(
        r.status.success(),
        "1MB-frame program failed to compile:\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    let asm = std::fs::read_to_string(&s_path).unwrap();
    let touches = asm.matches("orq $0, (%rsp)").count();
    assert!(
        touches >= 250,
        "~1MB frame should probe 250+ pages, saw {}",
        touches
    );
    let assemble = Command::new("as")
        .args(["--64", "-o"])
        .arg(dir.join("big1m.o"))
        .arg(&s_path)
        .output()
        .expect("cannot run as");
    assert!(
        assemble.status.success(),
        "as rejected the 1MB-frame asm:\n{}",
        String::from_utf8_lossy(&assemble.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
