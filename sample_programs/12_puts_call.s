.global _main
.text
.p2align 2

_main:
    adrp x0, msg@PAGE
    add x0, x0, msg@PAGEOFF
    bl _puts

    mov x0, #0
    bl _fflush

    mov x0, #0
    mov x16, #1
    svc #0x80

.section __TEXT,__cstring
msg:
    .asciz "Hello from puts()"
