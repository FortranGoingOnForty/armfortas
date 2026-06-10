.text
.globl add_two
.p2align 4
.type add_two,@function
add_two:
    pushq %rbp
    movq %rsp, %rbp
    movq %rdi, %rax
    addq %rsi, %rax
    movq %rbp, %rsp
    popq %rbp
    ret
.size add_two, .-add_two
