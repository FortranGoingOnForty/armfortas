.global _main
.text
.p2align 2

_main:
    adrp x1, lhs@PAGE
    add x1, x1, lhs@PAGEOFF
    adrp x2, rhs@PAGE
    add x2, x2, rhs@PAGEOFF
    mov x0, #0
    mov x3, #4
1:
    cbz x3, 2f
    ldr x4, [x1]
    ldr x5, [x2]
    mul x6, x4, x5
    add x0, x0, x6
    add x1, x1, #8
    add x2, x2, #8
    sub x3, x3, #1
    b 1b
2:
    mov x16, #1
    svc #0x80

.data
.p2align 3
lhs:
    .quad 1, 2, 3, 4
rhs:
    .quad 5, 6, 7, 8
