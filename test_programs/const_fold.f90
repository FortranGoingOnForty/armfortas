! Constant folding sanity check.
!
! Every printed value below is computable at compile time. The pass
! manager runs `const-fold` at -O1 and above, but the program must
! still produce the correct results at -O0 (where folding is off).
!
! CHECK: 7
! CHECK: 12
! CHECK: 6
! CHECK: 100
! CHECK: T
! CHECK: F
! CHECK: 6.28
program test_const_fold
    implicit none
    integer :: a, b, c, d
    real(8) :: x
    logical :: t, f

    a = 3 + 4
    b = 6 * 2
    c = 18 / 3
    d = 10 ** 2
    t = (5 == 5)
    f = (5 < 3)
    x = 2.0d0 * 3.14d0

    print *, a
    print *, b
    print *, c
    print *, d
    print *, t
    print *, f
    print *, x
end program
