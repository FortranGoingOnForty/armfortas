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
