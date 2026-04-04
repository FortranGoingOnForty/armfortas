! CHECK: ABCDE
! CHECK: XY
! CHECK: Hello
program test_fixed_multi
    implicit none
    character(5) :: a, b, c

    ! Multiple fixed-length variables — ensure no cross-contamination.
    a = 'ABCDE'
    b = 'XY'
    c = 'Hello World'

    print *, a
    print *, b
    print *, c
end program
