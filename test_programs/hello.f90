! STDOUT_EXACT: \x20Hello, World!\n
! STDERR_EXACT:
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Ofast => stdout|stderr|exit
program hello
    implicit none
    print *, 'Hello, World!'
end program
