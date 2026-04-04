! CHECK: 1
! CHECK: 1
! CHECK: 2
! CHECK: 3
! CHECK: 5
! CHECK: 8
! CHECK: 13
! CHECK: 21
! CHECK: 34
! CHECK: 55
program test_fib
    implicit none
    integer :: i
    do i = 1, 10
        print *, fib(i)
    end do
contains
    recursive function fib(n) result(f)
        integer, intent(in) :: n
        integer :: f
        if (n <= 1) then
            f = n
        else
            f = fib(n - 1) + fib(n - 2)
        end if
    end function
end program
