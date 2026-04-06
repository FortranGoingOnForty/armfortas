.global _main
.text
.p2align 2

_main:
    mov x0, #6
    mov x1, #7
    mul x0, x0, x1
    mov x16, #1
    svc #0x80
