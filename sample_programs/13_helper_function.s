.global _main
.text
.p2align 2

_main:
    mov x0, #7
    bl _triple
    mov x16, #1
    svc #0x80

_triple:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    mov x1, x0
    add x0, x0, x0
    add x0, x0, x1
    ldp x29, x30, [sp], #16
    ret
