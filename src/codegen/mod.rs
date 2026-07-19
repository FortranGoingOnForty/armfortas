//! Code generation.
//!
//! Per-arch backends live in submodules (`arm64/`, `x86/` from x03);
//! each owns its MIR, instruction selection, register allocation, and
//! emitter until x05 shows what the allocator genuinely shares.
//! Target-neutral identifiers live in `shared`.

pub mod arm64;
pub mod shared;
pub mod x86;

// x03 module reorg: the ARM64 backend moved into `arm64/`. These shims
// preserve the old paths so nothing outside the crate changes; new code
// should reference `codegen::arm64::*` directly.
pub use arm64::abi;
pub use arm64::emit;
pub use arm64::isel;
pub use arm64::linearscan;
pub use arm64::liveness;
pub use arm64::mir;
pub use arm64::peephole;
pub use arm64::regalloc;
pub use arm64::relax_branches;
pub use arm64::tailcall;

/// x86 allocator policy: linear-scan at EVERY opt level, naive only when
/// forced. The naive spill-everything allocator gives every vreg its own
/// stack slot, bloating frames ~6x and overflowing the stack on deep
/// recursion (fortsh's executor SIGSEGV'd at depth ~62) — gfortran does
/// real register allocation at -O0 too. `opt_level` is taken to make the
/// "no opt-level gate" invariant explicit and testable: re-adding an
/// `opt_level >= O1` condition here resurrects the bug, and
/// `x86_linear_scan_at_every_opt_level` would catch it.
fn x86_use_linear_scan(_opt_level: crate::driver::OptLevel, force_naive: bool) -> bool {
    !force_naive
}

/// Run the target backend over a lowered module and return assembly
/// text (x03). One closed `match` on arch: a missing arm is a compile
/// error; a trait-object registry would add indirection with no second
/// consumer.
pub fn emit_module(
    ir_module: &crate::ir::inst::Module,
    opts: &crate::driver::Options,
) -> Result<String, String> {
    match opts.target.arch {
        crate::target::Arch::Arm64 => Ok(arm64::emit_module(ir_module, opts)),
        crate::target::Arch::X86_64 => {
            let mut funcs = x86::isel::select_module(ir_module);
            if opts.verbose {
                eprintln!(" codegen: {} machine functions", funcs.len());
            }
            let mut text = String::new();
            for f in &mut funcs {
                // Backend peephole (O2+): same gate as the ARM64 one.
                if opts.opt_level >= crate::driver::OptLevel::O2 {
                    x86::peephole::run_peephole(f);
                }
                x86::twoaddr::convert_to_two_address(f);
                // Safety guard: a lowering size explosion (e.g. a routine
                // that inlines derived-type cleanup for dozens of types)
                // can produce a function whose dense liveness bitsets are
                // tens of GB — enough to OOM-kill the host. Reject it with
                // a clear error instead. 2 GiB is far above any sane
                // function; tripping this means the IR ballooned upstream.
                const LIVENESS_FOOTPRINT_CAP: u64 = 2 * 1024 * 1024 * 1024;
                let footprint = x86::liveness::liveness_footprint_bytes(f);
                if footprint > LIVENESS_FOOTPRINT_CAP {
                    return Err(format!(
                        "codegen: function '{}' is too large to register-allocate \
                         ({} basic blocks, {} instructions → {} MB of liveness \
                         bitsets, over the {} MB cap). This indicates an IR size \
                         explosion in lowering, not a normal program.",
                        f.name,
                        f.blocks.len(),
                        f.blocks.iter().map(|b| b.insts.len()).sum::<usize>(),
                        footprint / (1024 * 1024),
                        LIVENESS_FOOTPRINT_CAP / (1024 * 1024),
                    ));
                }
                let force_naive = std::env::var_os("ARMFORTAS_USE_NAIVE_REGALLOC").is_some();
                let use_linear = x86_use_linear_scan(opts.opt_level, force_naive);
                if use_linear {
                    let result = x86::linearscan::linear_scan(f);
                    x86::linearscan::apply_allocation(f, &result);
                } else {
                    x86::regalloc::regalloc_naive(f);
                }
                // Post-regalloc peephole (x10b, O2+): patterns over
                // physical registers and spill slots that only exist
                // after allocation (xor-zeroing, store-to-load
                // forwarding, lea folding). `ARMFORTAS_NO_POST_PEEP`
                // disables it — a bisection knob for peephole-induced
                // miscompiles, mirroring the allocator's env toggles.
                if opts.opt_level >= crate::driver::OptLevel::O2
                    && std::env::var_os("ARMFORTAS_NO_POST_PEEP").is_none()
                {
                    x86::peephole::run_peephole_post_regalloc(f);
                }
                text.push_str(&x86::emit::emit_function(f));
                for (label, bits) in &f.rodata {
                    text.push_str(&x86::emit::emit_rodata_f64(label, f64::from_bits(*bits)));
                }
                for (label, bytes) in &f.rodata_bytes {
                    text.push_str(&x86::emit::emit_rodata_bytes(label, bytes));
                }
                text.push('\n');
            }
            if !ir_module.globals.is_empty() {
                text.push_str(&shared::emit_globals(
                    &ir_module.globals,
                    &ir_module.layout,
                    shared::GlobalsDialect::Elf,
                ));
                text.push('\n');
            }
            // ELF entry wrapper: main → runtime init, program body,
            // finalize (the Mach-O twin lives in arm64::emit_module).
            if let Some(prog) = funcs.iter().find(|f| f.name.starts_with("__prog_")) {
                text.push_str(&format!(
                    "\
.text
.globl main
.p2align 4
.type main,@function
main:
    pushq %rbp
    movq %rsp, %rbp
    callq afs_program_init
    callq {0}
    callq afs_program_finalize
    xorl %eax, %eax
    movq %rbp, %rsp
    popq %rbp
    ret
.size main, .-main
",
                    prog.name
                ));
            }
            text.push_str(".section .note.GNU-stack,\"\",@progbits\n");
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{emit_module, x86_use_linear_scan};
    use crate::driver::{OptLevel, Options};
    use crate::ir::inst::Module;
    use crate::target::{TargetLayout, TargetSpec};

    /// The x86 backend must use linear-scan at EVERY opt level (the naive
    /// spill-everything allocator overflows the stack on deep recursion).
    /// Re-introducing an opt-level gate here is the exact way to regress
    /// that; this test forbids it.
    #[test]
    fn x86_linear_scan_at_every_opt_level() {
        for opt in [
            OptLevel::O0,
            OptLevel::O1,
            OptLevel::O2,
            OptLevel::O3,
            OptLevel::Os,
            OptLevel::Ofast,
        ] {
            assert!(
                x86_use_linear_scan(opt, false),
                "x86 must use linear-scan at {opt:?}"
            );
            assert!(
                !x86_use_linear_scan(opt, true),
                "ARMFORTAS_USE_NAIVE_REGALLOC must force naive at {opt:?}"
            );
        }
    }

    #[test]
    fn x86_modules_request_a_non_executable_stack() {
        let target = TargetSpec::parse("x86_64-linux-gnu").unwrap();
        let module = Module::new("empty".into(), TargetLayout::of(&target));
        let opts = Options {
            target,
            ..Options::default()
        };
        let asm = emit_module(&module, &opts).unwrap();
        assert!(asm.ends_with(".section .note.GNU-stack,\"\",@progbits\n"));
    }
}
