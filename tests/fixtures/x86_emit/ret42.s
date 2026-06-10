.text
.globl ret42
.p2align 4
.type ret42,@function
ret42:
    pushq %rbp
    movq %rsp, %rbp
    movl $42, %eax
    movq %rbp, %rsp
    popq %rbp
    ret
.size ret42, .-ret42
