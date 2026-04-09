! FLUSH stress test — the gfortran killer.
! Writes 1000 integers to a file, flushing after each write,
! then reads them all back and verifies the sum.
! This pattern causes crashes in gfortran on ARM64 due to
! buffered I/O state corruption under rapid flush cycles.
!
! CHECK: 500500
! FILE_EXISTS: afs_flush_stress.dat
! FILE_LINE_COUNT: afs_flush_stress.dat => 1000
! REPRO_CHECK: run_same_sandbox
program test_io_flush_stress
    implicit none
    integer :: i, val, total

    open(10, file='afs_flush_stress.dat', status='replace')
    do i = 1, 1000
        write(10, *) i
        flush(10)
    end do
    close(10)

    total = 0
    open(10, file='afs_flush_stress.dat', status='old')
    do i = 1, 1000
        read(10, *) val
        total = total + val
    end do
    close(10)

    print *, total
end program
