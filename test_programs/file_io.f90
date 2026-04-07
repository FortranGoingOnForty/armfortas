! File I/O roundtrip test.
! Tests: OPEN with status='replace', WRITE list-directed to file unit,
! CLOSE, reopen with status='old' for reading, READ list-directed.
! This exercises the unit table, file handles, BufWriter flush on close,
! and the default action inference (old→read, replace→write).
!
! Bug found during development: second OPEN with status='old' and no
! explicit action= was defaulting to 'readwrite', which opened a
! FileWrite (can't read). Fixed by inferring action from status.
!
! CHECK: 42
program test_file_io
    implicit none
    integer :: x
    open(10, file='afs_io_test.dat', status='replace')
    write(10, *) 42
    close(10)
    x = 0
    open(10, file='afs_io_test.dat', status='old')
    read(10, *) x
    close(10)
    print *, x
end program
