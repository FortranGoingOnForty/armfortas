.global _main
.text
.p2align 2

_main:
    mov x0, #48
    mov x1, #18
1:
    cmp x0, x1
    b.eq 2f
    b.gt 3f
    sub x1, x1, x0
    b 1b
3:
    sub x0, x0, x1
    b 1b
2:
    mov x16, #1
    svc #0x80
