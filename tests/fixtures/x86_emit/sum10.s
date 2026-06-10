.text
.globl sum10
.p2align 4
.type sum10,@function
sum10:
    pushq %rbp
    movq %rsp, %rbp
    movq $0, %rax
    movq $1, %rcx
    jmp .Lsum10_1
.Lsum10_1:
    addq %rcx, %rax
    addq $1, %rcx
    cmpq $10, %rcx
    jle .Lsum10_1
    jmp .Lsum10_2
.Lsum10_2:
    movq %rbp, %rsp
    popq %rbp
    ret
.size sum10, .-sum10
