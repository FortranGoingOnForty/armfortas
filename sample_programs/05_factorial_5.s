.global _main
.text
.p2align 2

_main:
    mov x0, #1
    mov x1, #5
1:
    mul x0, x0, x1
    sub x1, x1, #1
    cmp x1, #1
    b.gt 1b

    mov x16, #1
    svc #0x80
