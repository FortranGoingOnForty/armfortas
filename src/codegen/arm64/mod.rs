//! ARM64 backend: instruction selection, register allocation, stack
//! frame layout, and emission of assembly text for the afs-as
//! assembler (Mach-O conventions throughout).

pub mod abi;
pub mod emit;
pub mod isel;
pub mod linearscan;
pub mod liveness;
pub mod mir;
pub mod peephole;
pub mod regalloc;
pub mod relax_branches;
pub mod tailcall;

use crate::driver::{OptLevel, Options};
use crate::ir::inst::Module;

/// The Fortran PROGRAM body a `_main` wrapper should call, if any.
/// Only `__prog_*` functions qualify: wrapping any non-"main" function
/// incorrectly wrapped module procedures.
pub fn main_wrapper_target(allocated: &[mir::MachineFunction]) -> Option<&str> {
    allocated
        .iter()
        .find(|func| func.name.starts_with("__prog_"))
        .map(|func| func.name.as_str())
}

/// Select, allocate, and print the whole module as Mach-O ARM64
/// assembly. Moved verbatim from the driver's inline sequence (x03).
pub fn emit_module(ir_module: &Module, opts: &Options) -> String {
    // Instruction selection.
    let machine_funcs = isel::select_module(ir_module);

    // Backend peephole (O2+); floating-point contraction remains Ofast-only.
    let mut allocated: Vec<_> = machine_funcs;
    if opts.opt_level >= OptLevel::O2 {
        for mf in &mut allocated {
            peephole::run_peephole(mf, opts.opt_level.fp_contract());
        }
    }

    let use_naive_regalloc = opts.opt_level == OptLevel::O0
        || std::env::var_os("ARMFORTAS_USE_NAIVE_REGALLOC").is_some();

    // Register allocation.
    for mf in &mut allocated {
        if use_naive_regalloc {
            regalloc::regalloc_naive(mf);
        } else {
            let liveness = liveness::compute_liveness(mf);
            let result = linearscan::linear_scan(mf, &liveness);
            linearscan::apply_allocation(mf, &result, &liveness);
            linearscan::parallelize_entry_arg_moves(mf);
            linearscan::parallelize_call_arg_moves(mf);
            linearscan::insert_split_bridges(mf, &result.split_records);
            linearscan::insert_callee_saves(mf, &result.callee_saved_used);
            linearscan::coalesce_moves(mf);
            // Tail call optimization (O1+): BL + epilogue → epilogue + B.
            // Runs after regalloc so we can inspect physical register
            // assignments.
            if opts.opt_level >= OptLevel::O1 {
                tailcall::tail_call_opt(mf);
            }
        }
        // Branch relaxation: widen out-of-range B.cond/CBZ/TBZ-family
        // branches through an inverted short branch, and replace an
        // out-of-range local B with a position-independent x16 veneer.
        // Iterate to a fixed point because either rewrite changes layout.
        relax_branches::relax_branches(mf);
    }

    // Emit assembly.
    let mut asm_text = String::new();
    asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
    for mf in &allocated {
        // Re-emit __TEXT section before each function in case the previous
        // function's constant pool switched to __DATA.
        asm_text.push_str(".section __TEXT,__text,regular,pure_instructions\n");
        asm_text.push_str(&emit::emit_function(mf));
        asm_text.push('\n');
    }

    // Emit module-level globals (SAVE'd locals + module variables)
    // into a __DATA,__data section. Must come before _main so the
    // labels are defined when functions reference them.
    if !ir_module.globals.is_empty() {
        asm_text.push_str(&crate::codegen::shared::emit_globals(
            &ir_module.globals,
            &ir_module.layout,
            crate::codegen::shared::GlobalsDialect::MachO,
        ));
        asm_text.push('\n');
    }

    // Emit _main entry point (must be in __TEXT section).
    if let Some(user_func) = main_wrapper_target(&allocated) {
        if user_func != "main" {
            asm_text.push_str("\n.section __TEXT,__text,regular,pure_instructions\n");
            asm_text.push_str(&format!(
                "\
.globl _main
.p2align 2
_main:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl _afs_program_init
    bl _{0}
    bl _afs_program_finalize
    mov x0, #0
    ldp x29, x30, [sp], #16
    ret
",
                user_func
            ));
        }
    }

    if opts.verbose {
        eprintln!(" codegen: {} machine functions", allocated.len());
    }
    asm_text
}
