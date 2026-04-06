.global _main
.text
.p2align 2

_main:
    adrp x1, msg@PAGE
    add x1, x1, msg@PAGEOFF
    mov x0, #0
1:
    ldrb w2, [x1]
    cbz w2, 2f
    add x0, x0, #1
    add x1, x1, #1
    b 1b
2:
    mov x16, #1
    svc #0x80

.section __TEXT,__cstring
msg:
    .asciz "assembler"
