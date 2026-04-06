.global _main
.text
.p2align 2

_main:
    ldr x0, answer
    mov x16, #1
    svc #0x80

.p2align 3
answer:
    .quad 77
