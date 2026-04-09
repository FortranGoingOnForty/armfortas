! CHECK: Hello, World!
! REPRO_CHECK: asm
! REPRO_CHECK: obj
program hello
    implicit none
    print *, 'Hello, World!'
end program
