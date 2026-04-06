.global _main
.text
.p2align 2

_main:
    adrp x1, values@PAGE
    add x1, x1, values@PAGEOFF
    mov x0, #0
    mov x2, #8
1:
    cbz x2, 2f
    ldr x3, [x1]
    add x0, x0, x3
    add x1, x1, #8
    sub x2, x2, #1
    b 1b
2:
    mov x16, #1
    svc #0x80

.data
.p2align 3
values:
    .quad 1, 2, 3, 4, 5, 6, 7, 8
