//! Naive x86 register allocation + frame layout (sprint x05).
//!
//! Every vreg gets a stack slot; every instruction loads its uses into
//! scratch registers and stores its def back. The scratches — r10/r11
//! for GP, xmm14/xmm15 for FP — are caller-saved, outside the SysV
//! argument sequence (rdi..r9, xmm0-7), and outside every
//! fixed-register lowering (rax/rdx for idiv, rcx for shifts), so the
//! rewrite never collides with an explicit `Reg` operand the selector
//! placed. This is the x86 analog of `arm64::regalloc_naive`, which is
//! what the ARM backend runs at -O0; a linear scan joins the shared-
//! core question in x09 (see the x05 sprint-doc revision note).
//!
//! Flags discipline: the only instructions this pass inserts are
//! `mov` family loads/stores, which do not write RFLAGS, so a
//! cmp/ucomi stays adjacent (flag-wise) to its jcc/setcc consumer.
//!
//! Frame layout (rbp-based, x04 red-zone policy: rsp always moves):
//!
//! ```text
//!   rbp+8   return address
//!   rbp+0   saved rbp
//!   rbp-..  alloca slots, then vreg spill slots (8-byte aligned)
//!   rsp+..  outgoing call arguments (outgoing_arg_bytes)
//!   rsp     bottom
//! ```
//!
//! At entry rsp ≡ 8 (mod 16); after `push rbp` it is ≡ 0; the frame
//! subtraction is rounded to 16 so rsp ≡ 0 (mod 16) at every call.
//! No callee-saved registers are ever used here, so there is no push
//! parity to account for beyond rbp itself.

use std::collections::HashMap;

use super::mir::{
    OpSize, X86Function, X86Inst, X86Opcode, X86Operand, X86Reg, X86RegClass, X86VReg,
};
use crate::codegen::shared::VRegId;

const GP_SCRATCH: [X86Reg; 2] = [X86Reg::R10, X86Reg::R11];
const FP_SCRATCH: [X86Reg; 2] = [X86Reg::Xmm14, X86Reg::Xmm15];

/// Allocate every vreg to a frame slot and rewrite the function to use
/// scratch registers. Also lays out the frame: after this pass no
/// `VReg` or `FrameSlot` operands remain and `frame_bytes` is final.
pub fn regalloc_naive(f: &mut X86Function) {
    // ---- Phase 1: discover vregs (class travels on every operand
    // occurrence) and give each a spill slot.
    let mut vreg_class: HashMap<VRegId, X86RegClass> = HashMap::new();
    for block in &f.blocks {
        for inst in &block.insts {
            for op in inst.operands.iter().chain(inst.def.iter()) {
                if let X86Operand::VReg(v) = op {
                    vreg_class.insert(v.id, v.class);
                }
            }
        }
    }
    let mut vreg_slot: HashMap<VRegId, i32> = HashMap::new();
    let mut ids: Vec<VRegId> = vreg_class.keys().copied().collect();
    ids.sort(); // deterministic slot assignment
    for id in ids {
        let slot = f.alloc_frame_slot(8, 8);
        vreg_slot.insert(id, slot);
    }

    // ---- Phase 2: frame layout. Slot id → negative rbp offset.
    let mut offset: i64 = 0;
    let mut slot_disp: HashMap<i32, i64> = HashMap::new();
    for slot in &f.frame_slots {
        let align = slot.align.max(1) as i64;
        offset = -(((-offset) + slot.size as i64 + align - 1) & !(align - 1));
        slot_disp.insert(slot.id, offset);
    }
    let locals = -offset;
    let frame = (locals + f.outgoing_arg_bytes + 15) & !15;
    f.frame_bytes = frame;

    let mem_for_slot = |slot: i32| -> X86Operand {
        X86Operand::Mem {
            base: Some(X86Reg::Rbp),
            index: None,
            scale: 1,
            disp: *slot_disp
                .get(&slot)
                .unwrap_or_else(|| panic!("frame slot {} has no layout", slot)),
        }
    };

    // ---- Phase 3: rewrite instructions.
    for block_idx in 0..f.blocks.len() {
        let insts = std::mem::take(&mut f.blocks[block_idx].insts);
        let mut out = Vec::with_capacity(insts.len() * 3);
        for mut inst in insts {
            let mut gp_used = 0usize;
            let mut fp_used = 0usize;
            let mut scratch_for = |v: &X86VReg| -> X86Reg {
                match v.class {
                    X86RegClass::Xmm => {
                        let r = FP_SCRATCH[fp_used.min(1)];
                        fp_used += 1;
                        r
                    }
                    _ => {
                        let r = GP_SCRATCH[gp_used.min(1)];
                        gp_used += 1;
                        r
                    }
                }
            };

            // The tied case (post-twoaddr: def == operands[0]) must
            // land def and the tied use in the SAME scratch, loaded
            // once — handle it before the generic walk.
            let tied = inst.opcode.tied_use().is_some()
                && matches!((&inst.def, inst.operands.first()),
                    (Some(X86Operand::VReg(d)), Some(X86Operand::VReg(a))) if d.id == a.id);

            let mut def_store: Option<(X86Reg, X86RegClass, i32)> = None;

            if tied {
                let v = match inst.operands[0] {
                    X86Operand::VReg(v) => v,
                    _ => unreachable!(),
                };
                let scratch = scratch_for(&v);
                let slot = vreg_slot[&v.id];
                out.push(load(scratch, v.class, mem_for_slot(slot), inst.size));
                inst.operands[0] = X86Operand::Reg(scratch);
                inst.def = Some(X86Operand::Reg(scratch));
                def_store = Some((scratch, v.class, slot));
            }

            // Uses: every remaining VReg operand loads into a scratch.
            // Address-in-vreg loads (the MovRM convention) become
            // Mem{base: scratch} instead of Reg(scratch) — a Reg there
            // would print a register move, not a memory access.
            let addr_position = addr_operand_position(&inst);
            let (xmm_use_width, xmm_def_width) = xmm_width_override(inst.opcode);
            for (i, op) in inst.operands.iter_mut().enumerate() {
                if tied && i == 0 {
                    continue;
                }
                match op {
                    X86Operand::VReg(v) => {
                        let scratch = scratch_for(v);
                        let slot = vreg_slot[&v.id];
                        // Addresses are always 8-byte loads regardless
                        // of the instruction's operand size.
                        let load_size = if Some(i) == addr_position {
                            OpSize::Q
                        } else if v.class == X86RegClass::Xmm {
                            xmm_use_width.unwrap_or(inst.size)
                        } else {
                            inst.size
                        };
                        out.push(load(scratch, v.class, mem_for_slot(slot), load_size));
                        *op = if Some(i) == addr_position {
                            X86Operand::Mem {
                                base: Some(scratch),
                                index: None,
                                scale: 1,
                                disp: 0,
                            }
                        } else {
                            X86Operand::Reg(scratch)
                        };
                    }
                    X86Operand::FrameSlot(slot) => {
                        *op = mem_for_slot(*slot);
                    }
                    _ => {}
                }
            }

            // Def: a VReg def executes into a scratch and stores back.
            // A class mismatch on Movss/Movsd (Gp64 def on an FP move)
            // is the documented FP-store-to-address form: the def is an
            // address, not a destination register.
            if !tied {
                match inst.def.clone() {
                    Some(X86Operand::VReg(v)) => {
                        let is_fp_store_addr =
                            matches!(inst.opcode, X86Opcode::Movss | X86Opcode::Movsd)
                                && v.class != X86RegClass::Xmm;
                        let scratch = scratch_for(&v);
                        let slot = vreg_slot[&v.id];
                        if is_fp_store_addr {
                            out.push(load(scratch, v.class, mem_for_slot(slot), OpSize::Q));
                            inst.def = Some(X86Operand::Mem {
                                base: Some(scratch),
                                index: None,
                                scale: 1,
                                disp: 0,
                            });
                        } else {
                            inst.def = Some(X86Operand::Reg(scratch));
                            def_store = Some((scratch, v.class, slot));
                        }
                    }
                    Some(X86Operand::FrameSlot(slot)) => {
                        inst.def = Some(mem_for_slot(slot));
                    }
                    _ => {}
                }
            }

            let size = inst.size;
            out.push(inst);
            if let Some((scratch, class, slot)) = def_store {
                let store_size = if class == X86RegClass::Xmm {
                    xmm_def_width.unwrap_or(size)
                } else {
                    size
                };
                out.push(store(scratch, class, mem_for_slot(slot), store_size));
            }
        }
        f.blocks[block_idx].insts = out;
    }
}

/// Conversion opcodes carry the GP-side width in `inst.size` (that is
/// what the l/q suffix describes); the XMM side has a fixed width of
/// its own. Without this override the spill code sizes the FP slot
/// traffic off the GP suffix: `cvtsi2sdl` stored its double def with
/// movss (top 4 bytes of the slot stale) and `cvttsd2sil` loaded its
/// double source with movss (top half zeroed) — both silent wrong
/// answers at runtime. Returns (use_width, def_width) for XMM-class
/// operands.
fn xmm_width_override(opcode: X86Opcode) -> (Option<OpSize>, Option<OpSize>) {
    match opcode {
        X86Opcode::Cvtsi2ss => (None, Some(OpSize::L)),
        X86Opcode::Cvtsi2sd => (None, Some(OpSize::Q)),
        X86Opcode::Cvttss2si => (Some(OpSize::L), None),
        X86Opcode::Cvttsd2si => (Some(OpSize::Q), None),
        X86Opcode::Cvtss2sd => (Some(OpSize::L), Some(OpSize::Q)),
        X86Opcode::Cvtsd2ss => (Some(OpSize::Q), Some(OpSize::L)),
        _ => (None, None),
    }
}

/// Which operand index, if any, carries an address-in-vreg (the
/// `X86Opcode::MovRM` convention) or address-as-def store form.
fn addr_operand_position(inst: &X86Inst) -> Option<usize> {
    match inst.opcode {
        // MovRM and the extending loads: operand 0 is the source address.
        X86Opcode::MovRM | X86Opcode::MovzxRM { .. } | X86Opcode::MovsxRM { .. } => Some(0),
        // MovMR/MovMI: operand 1 is the destination address.
        X86Opcode::MovMR | X86Opcode::MovMI => Some(1),
        // Lea reads an address operand (FrameSlot/Mem/RipLabel), never
        // dereferences a vreg address — but a vreg there would still
        // be an address value.
        X86Opcode::Lea => Some(0),
        _ => None,
    }
}

fn load(scratch: X86Reg, class: X86RegClass, mem: X86Operand, size: OpSize) -> X86Inst {
    match class {
        X86RegClass::Xmm => X86Inst {
            opcode: if size == OpSize::L {
                X86Opcode::Movss
            } else {
                X86Opcode::Movsd
            },
            size,
            operands: vec![mem],
            def: Some(X86Operand::Reg(scratch)),
        },
        _ => X86Inst {
            opcode: X86Opcode::MovRM,
            size,
            operands: vec![mem],
            def: Some(X86Operand::Reg(scratch)),
        },
    }
}

fn store(scratch: X86Reg, class: X86RegClass, mem: X86Operand, size: OpSize) -> X86Inst {
    match class {
        X86RegClass::Xmm => X86Inst {
            opcode: if size == OpSize::L {
                X86Opcode::Movss
            } else {
                X86Opcode::Movsd
            },
            size,
            operands: vec![X86Operand::Reg(scratch)],
            def: Some(mem),
        },
        _ => X86Inst {
            opcode: X86Opcode::MovMR,
            size,
            operands: vec![X86Operand::Reg(scratch), mem],
            def: None,
        },
    }
}
