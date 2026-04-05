! Formatted integer WRITE test.
! Tests: I edit descriptor with width.
!
! CHECK:    42
program test_io_format_int
    implicit none
    integer :: x
    x = 42
    write(*, '(I5)') x
end program
