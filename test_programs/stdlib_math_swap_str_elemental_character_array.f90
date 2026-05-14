! CHECK: ok
! REPRO_CHECK: run
module m
  implicit none

  interface swap
    module procedure swap_str
  end interface
contains
  elemental subroutine swap_str(lhs, rhs)
    character(*), intent(inout) :: lhs, rhs
    character(len=max(len(lhs), len(rhs))) :: temp

    temp = lhs
    lhs = rhs
    rhs = temp
  end subroutine
end module

program main
  use m
  implicit none

  block
    character(5) :: x(2), y(2)

    x = ['abcde', 'fghij']
    y = ['fghij', 'abcde']

    call swap(x, y)

    if (.not. all(x == ['fghij', 'abcde'])) error stop 1
    if (.not. all(y == ['abcde', 'fghij'])) error stop 2
    call swap(x, x)
    if (.not. all(x == ['fghij', 'abcde'])) error stop 3
  end block

  block
    character(4) :: x
    character(6) :: y

    x = 'abcd'
    y = 'efghij'
    call swap(x, y)
    if (x /= 'efgh') error stop 4
    if (y /= 'abcd  ') error stop 5

    x = 'abcd'
    y = 'efghij'
    call swap(x, y(1:4))
    if (x /= 'efgh') error stop 6
    if (y /= 'abcdij') error stop 7
  end block

  print *, 'ok'
end program
