.global _main
.text
.p2align 2

_main:
    mov x0, #0
    mov x1, #1
1:
    add x0, x0, x1
    add x1, x1, #1
    cmp x1, #11
    b.lt 1b

    mov x16, #1
    svc #0x80
