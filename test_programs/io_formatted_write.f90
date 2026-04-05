! Formatted WRITE test.
! Tests: WRITE with format string (not list-directed).
! Exercises: format engine integration through the full pipeline.
!
! CHECK: hello world
program test_io_formatted_write
    implicit none
    write(*, '(A)') 'hello world'
end program
