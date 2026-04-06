! LICM target: a constant computation inside the loop body that does
! not depend on the induction variable. -O2 should hoist `c * 2` out
! of the loop into the preheader.
!
! CHECK: 460
program test_licm
    implicit none
    integer :: i, sum, c
    c = 23
    sum = 0
    do i = 1, 10
        sum = sum + c * 2
    end do
    print *, sum
end program
