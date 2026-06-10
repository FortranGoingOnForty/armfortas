//! Sprint x03 golden tests for the x86_64 AT&T/ELF emitter.
//!
//! Hand-built machine functions over physical registers, string-compared
//! against checked-in fixtures (pure text — runs on macOS too). The
//! assembler-acceptance test additionally runs system `as` and checks
//! the object header, on x86_64 ELF hosts only.
//!
//! Regenerate fixtures with
//! `AFS_UPDATE_GOLDEN=1 cargo test --test x86_emit_golden`.

use std::path::PathBuf;
use std::process::Command;

use armfortas::codegen::shared::MBlockId;
use armfortas::codegen::x86::emit;
use armfortas::codegen::x86::mir::{
    OpSize, X86Block, X86Cond, X86Function, X86Inst, X86Opcode, X86Operand, X86Reg,
};
use armfortas::target::{Arch, ObjectFormat, TargetSpec};

fn inst(
    opcode: X86Opcode,
    size: OpSize,
    operands: Vec<X86Operand>,
    def: Option<X86Operand>,
) -> X86Inst {
    X86Inst {
        opcode,
        size,
        operands,
        def,
    }
}

fn reg(r: X86Reg) -> X86Operand {
    X86Operand::Reg(r)
}

/// `ret42`: mov $42, %eax; ret.
fn fixture_ret42() -> String {
    let mut f = X86Function::new("ret42".into());
    f.blocks[0].insts = vec![
        inst(
            X86Opcode::MovRI,
            OpSize::L,
            vec![X86Operand::Imm(42)],
            Some(reg(X86Reg::Rax)),
        ),
        inst(X86Opcode::Ret, OpSize::Q, vec![], None),
    ];
    emit::emit_function(&f)
}

/// `add_two(a=%rdi, b=%rsi)`: result in %rax, tie satisfied via the
/// mov-then-op rewrite x05 will automate.
fn fixture_add_two() -> String {
    let mut f = X86Function::new("add_two".into());
    f.blocks[0].insts = vec![
        inst(
            X86Opcode::MovRR,
            OpSize::Q,
            vec![reg(X86Reg::Rdi)],
            Some(reg(X86Reg::Rax)),
        ),
        inst(
            X86Opcode::Add,
            OpSize::Q,
            vec![reg(X86Reg::Rax), reg(X86Reg::Rsi)],
            Some(reg(X86Reg::Rax)),
        ),
        inst(X86Opcode::Ret, OpSize::Q, vec![], None),
    ];
    emit::emit_function(&f)
}

/// `sum10`: sum 1..=10 with a compare-and-branch loop over three
/// blocks. Pins the cmp operand order: `cmpq $10, %rcx; jle` loops
/// while %rcx <= 10.
fn fixture_sum10() -> String {
    let mut f = X86Function::new("sum10".into());
    // block 0: init
    f.blocks[0].insts = vec![
        inst(
            X86Opcode::MovRI,
            OpSize::Q,
            vec![X86Operand::Imm(0)],
            Some(reg(X86Reg::Rax)),
        ),
        inst(
            X86Opcode::MovRI,
            OpSize::Q,
            vec![X86Operand::Imm(1)],
            Some(reg(X86Reg::Rcx)),
        ),
        inst(
            X86Opcode::Jmp,
            OpSize::Q,
            vec![X86Operand::BlockRef(MBlockId(1))],
            None,
        ),
    ];
    // block 1: body — rax += rcx; rcx += 1; loop while rcx <= 10
    f.blocks.push(X86Block {
        id: MBlockId(1),
        insts: vec![
            inst(
                X86Opcode::Add,
                OpSize::Q,
                vec![reg(X86Reg::Rax), reg(X86Reg::Rcx)],
                Some(reg(X86Reg::Rax)),
            ),
            inst(
                X86Opcode::Add,
                OpSize::Q,
                vec![reg(X86Reg::Rcx), X86Operand::Imm(1)],
                Some(reg(X86Reg::Rcx)),
            ),
            inst(
                X86Opcode::Cmp,
                OpSize::Q,
                vec![reg(X86Reg::Rcx), X86Operand::Imm(10)],
                None,
            ),
            inst(
                X86Opcode::Jcc,
                OpSize::Q,
                vec![
                    X86Operand::Cond(X86Cond::Le),
                    X86Operand::BlockRef(MBlockId(1)),
                ],
                None,
            ),
            inst(
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(MBlockId(2))],
                None,
            ),
        ],
    });
    // block 2: ret
    f.blocks.push(X86Block {
        id: MBlockId(2),
        insts: vec![inst(X86Opcode::Ret, OpSize::Q, vec![], None)],
    });
    emit::emit_function(&f)
}

/// A function reading a `.rodata` f64 constant rip-relatively and a
/// `.data` global, plus the data directives themselves.
fn fixture_const_and_global() -> String {
    let mut out = String::new();
    let mut f = X86Function::new("scale_counter".into());
    f.blocks[0].insts = vec![
        inst(
            X86Opcode::Movsd,
            OpSize::Q,
            vec![X86Operand::RipLabel(".Lc_half".into())],
            Some(reg(X86Reg::Xmm0)),
        ),
        inst(
            X86Opcode::MovRM,
            OpSize::Q,
            vec![X86Operand::RipLabel("counter".into())],
            Some(reg(X86Reg::Rax)),
        ),
        inst(
            X86Opcode::Cvtsi2sd,
            OpSize::Q,
            vec![reg(X86Reg::Rax)],
            Some(reg(X86Reg::Xmm1)),
        ),
        inst(
            X86Opcode::Mulsd,
            OpSize::Q,
            vec![reg(X86Reg::Xmm0), reg(X86Reg::Xmm1)],
            Some(reg(X86Reg::Xmm0)),
        ),
        inst(X86Opcode::Ret, OpSize::Q, vec![], None),
    ];
    out.push_str(&emit::emit_function(&f));
    out.push('\n');
    out.push_str(&emit::emit_rodata_f64(".Lc_half", 0.5));
    out.push('\n');
    out.push_str(&emit::emit_data_quad("counter", 7, true));
    out
}

fn fixtures() -> Vec<(&'static str, String)> {
    vec![
        ("ret42.s", fixture_ret42()),
        ("add_two.s", fixture_add_two()),
        ("sum10.s", fixture_sum10()),
        ("const_and_global.s", fixture_const_and_global()),
    ]
}

fn fixture_dir() -> PathBuf {
    for base in ["tests/fixtures/x86_emit", "../tests/fixtures/x86_emit"] {
        let dir = PathBuf::from(base);
        if dir.parent().map(|p| p.exists()).unwrap_or(false) {
            return dir;
        }
    }
    PathBuf::from("tests/fixtures/x86_emit")
}

#[test]
fn x86_emit_matches_goldens() {
    let dir = fixture_dir();
    if std::env::var_os("AFS_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in fixtures() {
            std::fs::write(dir.join(name), text).unwrap();
        }
        eprintln!("x86 emit goldens regenerated in {}", dir.display());
        return;
    }
    for (name, text) in fixtures() {
        let path = dir.join(name);
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "cannot read {} ({}); run AFS_UPDATE_GOLDEN=1 cargo test --test x86_emit_golden",
                path.display(),
                e
            )
        });
        assert_eq!(text, golden, "{} diverged from its golden", name);
    }
}

/// x03 DoD grep gate: no Mach-O leaks. Underscore-prefixed symbols and
/// `.section __` directives survive visual review; this test does not.
#[test]
fn x86_emit_has_no_macho_conventions() {
    for (name, text) in fixtures() {
        for line in text.lines() {
            let t = line.trim();
            assert!(
                !t.contains(".section __"),
                "{}: Mach-O section directive leaked: {}",
                name,
                line
            );
            assert!(
                !t.starts_with(".align"),
                "{}: `.align` is bytes on x86 gas — use .p2align: {}",
                name,
                line
            );
            // Underscore-prefixed *symbols*: label definitions or
            // globl/call references starting with `_`.
            if let Some(rest) = t.strip_prefix(".globl ") {
                assert!(
                    !rest.starts_with('_'),
                    "{}: underscored symbol: {}",
                    name,
                    line
                );
            }
            if t.ends_with(':') && !t.starts_with('.') {
                assert!(!t.starts_with('_'), "{}: underscored label: {}", name, line);
            }
        }
    }
}

/// Pins the AT&T comparison operand order: MIR `Cmp(lhs, rhs)` prints
/// rhs first, so `cmpq $10, %rcx; jle` branches while %rcx <= 10.
#[test]
fn cmp_operand_order_is_att() {
    let text = fixture_sum10();
    assert!(
        text.contains("cmpq $10, %rcx"),
        "cmp must print rhs-first AT&T order:\n{}",
        text
    );
}

#[test]
#[should_panic(expected = "violated tie")]
fn violated_tie_panics_instead_of_printing() {
    let mut f = X86Function::new("bad_tie".into());
    f.blocks[0].insts = vec![inst(
        X86Opcode::Add,
        OpSize::Q,
        vec![reg(X86Reg::Rdi), reg(X86Reg::Rsi)],
        Some(reg(X86Reg::Rax)), // def != tied operand 0
    )];
    emit::emit_function(&f);
}

/// Assembler acceptance: on an x86_64 ELF host, system `as` must accept
/// every golden and produce an EM_X86_64 relocatable.
#[test]
fn x86_goldens_accepted_by_system_assembler() {
    let host = TargetSpec::host();
    if host.arch != Arch::X86_64 || host.object_format() != ObjectFormat::Elf {
        eprintln!(
            "\nHARNESS_SKIP suite=x86_emit_golden test=x86_goldens_accepted_by_system_assembler count={} reason=\"needs an x86_64 ELF host with system as\"",
            fixtures().len()
        );
        return;
    }
    let readelf =
        armfortas::testing::find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]);
    let dir = std::env::temp_dir().join(format!("afs_x86_accept_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for (name, text) in fixtures() {
        let s_path = dir.join(name);
        let o_path = s_path.with_extension("o");
        std::fs::write(&s_path, &text).unwrap();
        let out = Command::new("as")
            .args(["--64", "-o"])
            .arg(&o_path)
            .arg(&s_path)
            .output()
            .expect("cannot run system as");
        assert!(
            out.status.success(),
            "{}: system as rejected the emitted text:\n{}\n--- input ---\n{}",
            name,
            String::from_utf8_lossy(&out.stderr),
            text
        );
        let hdr = Command::new(&readelf)
            .arg("-h")
            .arg(&o_path)
            .output()
            .expect("cannot run readelf");
        let hdr_text = String::from_utf8_lossy(&hdr.stdout);
        assert!(
            hdr_text.contains("X86-64"),
            "{}: object is not EM_X86_64:\n{}",
            name,
            hdr_text
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
