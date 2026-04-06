.global _main
.text
.p2align 2

_main:
    mov x0, #9
    scvtf d0, x0
    mov x1, #16
    scvtf d1, x1
    fadd d2, d0, d1
    fsqrt d3, d2
    fcvtzs x0, d3
    mov x16, #1
    svc #0x80
