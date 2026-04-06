.global _main
.text
.p2align 2

_main:
    mov x0, #0
    mov x1, #1
    mov x2, #10
1:
    cbz x2, 2f
    add x3, x0, x1
    mov x0, x1
    mov x1, x3
    sub x2, x2, #1
    b 1b
2:
    mov x16, #1
    svc #0x80
