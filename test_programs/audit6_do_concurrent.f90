! Audit #6 probe — DO CONCURRENT.
!
! F2008 parallel-execution-permitted loop. Even without
! parallelization, the lowering must produce a correct
! sequential loop body and we want a regression test that the
! USE-ONLY filter walker added in audit5 MAJOR-2 still walks
! into DoConcurrent (it was missing from the original walker).
!
! CHECK: 2 4 6 8 10 12 14 16 18 20
program audit6_do_concurrent
  integer :: i, arr(10)
  do concurrent (i = 1:10)
    arr(i) = i * 2
  end do
  print *, arr
end program
