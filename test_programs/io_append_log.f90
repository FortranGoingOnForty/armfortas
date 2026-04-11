! Append-intent file oracle test.
! Writes one line per run to a persistent log file.
! Exercises: explicit append semantics across same-sandbox reruns.
!
! CHECK: 7
! FILE_EXISTS: afs_append.log
! FILE_LINE_COUNT: afs_append.log => 1
! FILE_RERUN_MODE: afs_append.log => append
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program test_io_append_log
    implicit none

    open(10, file='afs_append.log', status='unknown', position='append')
    write(10, *) 7
    close(10)

    print *, 7
end program
