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
            let mut text = String::new();
            for f in &mut funcs {
                x86::twoaddr::convert_to_two_address(f);
                x86::regalloc::regalloc_naive(f);
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
            Ok(text)
        }
    }
}
