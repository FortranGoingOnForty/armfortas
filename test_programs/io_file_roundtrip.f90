! Tests file I/O: write multiple values, close, reopen, read back.
! Exercises: OPEN status='replace', WRITE list-directed, CLOSE,
! OPEN status='old', READ list-directed, value verification.
!
! Bug history: second OPEN with status='old' defaulted to action='readwrite'
! which opened a FileWrite (can't read). Fixed by inferring action from status.
!
! Tests file I/O roundtrip: write, close, reopen, read.
!
! CHECK: 42
program test_io_file_roundtrip
    implicit none
    integer :: x

    open(10, file='/tmp/afs_rt_test.dat', status='replace')
    write(10, *) 42
    close(10)

    x = 0
    open(10, file='/tmp/afs_rt_test.dat', status='old')
    read(10, *) x
    close(10)

    print *, x
end program
