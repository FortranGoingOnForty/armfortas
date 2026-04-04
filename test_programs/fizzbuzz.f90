! CHECK: 1
! CHECK: 2
! CHECK: Fizz
! CHECK: 4
! CHECK: Buzz
! CHECK: Fizz
! CHECK: 7
! CHECK: 8
! CHECK: Fizz
! CHECK: Buzz
! CHECK: 11
! CHECK: Fizz
! CHECK: 13
! CHECK: 14
! CHECK: FizzBuzz
! CHECK: 16
! CHECK: 17
! CHECK: Fizz
! CHECK: 19
! CHECK: Buzz
program fizzbuzz
    implicit none
    integer :: i
    do i = 1, 20
        if (mod(i, 15) == 0) then
            print *, 'FizzBuzz'
        else if (mod(i, 3) == 0) then
            print *, 'Fizz'
        else if (mod(i, 5) == 0) then
            print *, 'Buzz'
        else
            print *, i
        end if
    end do
end program
