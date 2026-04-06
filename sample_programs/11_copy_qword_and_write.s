.global _main
.text
.p2align 2

_main:
    adrp x0, src@PAGE
    add x0, x0, src@PAGEOFF
    adrp x1, dst@PAGE
    add x1, x1, dst@PAGEOFF
    ldr x4, [x0]
    str x4, [x1]

    mov x0, #1
    mov x2, #8
    mov x16, #4
    svc #0x80

    mov x0, #0
    mov x16, #1
    svc #0x80

.data
.p2align 3
src:
    .ascii "Copy OK\n"

.section __DATA,__bss
.p2align 3
dst:
    .space 8
