.global _main
.text
.p2align 2

_main:
    mov x0, #0
    mov x16, #1
    svc #0x80
