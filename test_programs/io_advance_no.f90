! ADVANCE='NO' test.
! Tests: formatted write without trailing newline, followed by another write.
! Exercises: non-advancing I/O through the full pipeline.
!
! CHECK: helloworld
program test_io_advance_no
    implicit none
    write(*, '(A)', advance='no') 'hello'
    write(*, '(A)') 'world'
end program
