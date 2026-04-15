# ARM64 (AArch64) ISA Overview

Reference: ARM Architecture Reference Manual for A-profile architecture (ARMv8-A / ARMv9)

## Instruction Format
All ARM64 instructions are **fixed 32-bit (4 bytes)** wide. This regularity makes encoding and decoding straightforward compared to x86's variable-length encoding.

The top bits of each instruction determine the encoding class:
```
[31]    = sf (0=32-bit W, 1=64-bit X for many instructions)
[30:25] = major opcode group
[24:0]  = varies by group
```

## Register File

### General Purpose (31 registers + SP/ZR)
- **X0-X30**: 64-bit general purpose
- **W0-W30**: Lower 32 bits of X0-X30 (writing W clears upper 32 bits)
- **X31/SP/XZR**: Context-dependent — stack pointer or zero register
  - As source: zero register (XZR/WZR)
  - As SP context: stack pointer

### Floating-Point / SIMD (32 registers)
- **D0-D31**: 64-bit double precision
- **S0-S31**: 32-bit single precision (lower half of D)
- **H0-H31**: 16-bit half precision
- **Q0-Q31**: 128-bit SIMD/NEON
- **V0-V31**: Vector register (alias for Q, used with arrangement specifiers like V0.2D, V0.4S)

### Special
- **PC**: Program counter (not directly accessible in most instructions)
- **NZCV**: Condition flags (Negative, Zero, Carry, Overflow)
- **FPCR/FPSR**: Floating-point control/status

## Instruction Classes

### Data Processing — Register
```
ADD  Xd, Xn, Xm          ; Xd = Xn + Xm
SUB  Xd, Xn, Xm          ; Xd = Xn - Xm
MUL  Xd, Xn, Xm          ; Xd = Xn * Xm
SDIV Xd, Xn, Xm          ; Xd = Xn / Xm (signed)
AND  Xd, Xn, Xm          ; bitwise AND
ORR  Xd, Xn, Xm          ; bitwise OR
EOR  Xd, Xn, Xm          ; bitwise XOR
```
Encoding: `sf|opc|01011|shift|0|Rm|imm6|Rn|Rd`

### Data Processing — Immediate
```
ADD  Xd, Xn, #imm12      ; 12-bit immediate, optionally shifted left 12
MOV  Xd, #imm16           ; alias for MOVZ
MOVZ Xd, #imm16, LSL #n  ; move 16-bit immediate to position n (0,16,32,48)
MOVK Xd, #imm16, LSL #n  ; move and keep other bits
```

### Branching
```
B    label                 ; unconditional branch (±128MB range, 26-bit offset)
BL   label                 ; branch and link (call): X30 = return address
B.cond label               ; conditional branch (±1MB range, 19-bit offset)
CBZ  Xn, label             ; branch if Xn == 0
CBNZ Xn, label             ; branch if Xn != 0
TBZ  Xn, #bit, label       ; branch if bit is zero
RET  {Xn}                  ; return (branch to Xn, default X30)
BR   Xn                    ; indirect branch
BLR  Xn                    ; indirect call
```

### Comparison
```
CMP  Xn, Xm               ; alias: SUBS XZR, Xn, Xm (sets NZCV flags)
CMN  Xn, Xm               ; alias: ADDS XZR, Xn, Xm
TST  Xn, Xm               ; alias: ANDS XZR, Xn, Xm
```

### Condition Codes
```
EQ (Z=1)    NE (Z=0)      ; equal / not equal
LT (N!=V)   GE (N==V)     ; signed less / greater-or-equal
LE (Z=1||N!=V)  GT (Z=0&&N==V)  ; signed less-or-equal / greater
MI (N=1)    PL (N=0)      ; negative / positive
CS/HS (C=1) CC/LO (C=0)   ; carry set / carry clear (unsigned >= / <)
HI (C=1&&Z=0) LS (C=0||Z=1)  ; unsigned > / <=
VS (V=1)    VC (V=0)      ; overflow / no overflow
AL           NV            ; always / always (for nop-like encoding)
```

### Load/Store
```
LDR  Xd, [Xn]             ; load 64-bit from address in Xn
LDR  Xd, [Xn, #offset]   ; base + unsigned offset
LDR  Xd, [Xn, #offset]!  ; pre-index: Xn = Xn + offset, then load
LDR  Xd, [Xn], #offset   ; post-index: load, then Xn = Xn + offset
LDR  Xd, [Xn, Xm]        ; register offset
LDP  Xd1, Xd2, [Xn, #off]; load pair (two registers at once)
STR  Xd, [Xn, #offset]   ; store 64-bit
STP  Xd1, Xd2, [Xn, #off]; store pair

; Size variants: LDRB (byte), LDRH (halfword), LDRSB/LDRSH/LDRSW (sign-extend)
```

### Floating Point
```
FADD Dd, Dn, Dm           ; double add
FSUB Dd, Dn, Dm           ; double subtract
FMUL Dd, Dn, Dm           ; double multiply
FDIV Dd, Dn, Dm           ; double divide
FSQRT Dd, Dn              ; square root
FABS Dd, Dn               ; absolute value
FNEG Dd, Dn               ; negate
FMADD Dd, Dn, Dm, Da      ; fused multiply-add: Dd = Da + Dn*Dm
FCMP Dn, Dm               ; compare (sets NZCV)
FCVTZS Xd, Dn             ; float→int (truncate toward zero)
SCVTF Dd, Xn              ; int→float
FMOV Xd, Dn               ; move between GP and FP registers
```

### NEON/SIMD (for vectorization)
```
FADD V0.2D, V1.2D, V2.2D  ; 2x double add
FADD V0.4S, V1.4S, V2.4S  ; 4x float add
ADD  V0.4S, V1.4S, V2.4S  ; 4x int32 add
; etc. — same operations but on vector lanes
```

### System
```
SVC  #imm16               ; supervisor call (syscall)
NOP                        ; no operation
BRK  #imm16               ; breakpoint
```

## Key Encoding Patterns for afs-as

### Constant Materialization
ARM64 can only encode limited immediates directly. To load arbitrary 64-bit constants:
```
; Load 0x1234_5678_9ABC_DEF0:
MOVZ X0, #0xDEF0           ; X0 = 0x000000000000DEF0
MOVK X0, #0x9ABC, LSL #16  ; X0 = 0x000000009ABCDEF0
MOVK X0, #0x5678, LSL #32  ; X0 = 0x00005678_9ABCDEF0
MOVK X0, #0x1234, LSL #48  ; X0 = 0x12345678_9ABCDEF0
```

### PC-Relative Addressing (ADRP + ADD/LDR)
Access global data:
```
ADRP X0, symbol@PAGE        ; X0 = page containing symbol (4KB aligned)
ADD  X0, X0, symbol@PAGEOFF ; X0 = exact address of symbol
; or
LDR  X0, [X0, symbol@PAGEOFF] ; load value at symbol's address
```
This two-instruction pattern is the standard way to access globals on ARM64.

## Comparison with x86-64
| Feature | ARM64 | x86-64 |
|---------|-------|--------|
| Instruction size | Fixed 32-bit | Variable 1-15 bytes |
| Registers (GP) | 31 | 16 |
| Registers (FP) | 32 | 16 (XMM/YMM) |
| Load/store | Separate instructions | Memory operands in most instructions |
| Condition codes | Set by CMP/CMN/ADDS/SUBS | Set by most arithmetic |
| Encoding regularity | Very regular | Very irregular |

ARM64's regularity makes our assembler job much easier than it would be for x86.
