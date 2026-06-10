.text
.globl scale_counter
.p2align 4
.type scale_counter,@function
scale_counter:
    pushq %rbp
    movq %rsp, %rbp
    movsd .Lc_half(%rip), %xmm0
    movq counter(%rip), %rax
    cvtsi2sdq %rax, %xmm1
    mulsd %xmm1, %xmm0
    movq %rbp, %rsp
    popq %rbp
    ret
.size scale_counter, .-scale_counter

.section .rodata
.p2align 3
.Lc_half:
    .quad 0x3fe0000000000000

.data
.globl counter
.hidden counter
.p2align 3
.type counter,@object
.size counter, 8
counter:
    .quad 7
