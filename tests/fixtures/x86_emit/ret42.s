.text
.globl ret42
.p2align 4
.type ret42,@function
ret42:
    movl $42, %eax
    ret
.size ret42, .-ret42
