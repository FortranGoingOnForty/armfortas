.text
.globl add_two
.p2align 4
.type add_two,@function
add_two:
    movq %rdi, %rax
    addq %rsi, %rax
    ret
.size add_two, .-add_two
