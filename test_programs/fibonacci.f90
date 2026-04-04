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
