! Audit #4 BLOCKING-2 — 1-D slice print with negative stride.
!
! The Maj-4 fix from audit #3 added a bounded loop for slice
! prints, but hardcoded `cur > end → exit` which is wrong for a
! descending stride. `i=5, i>1 → true → exit` skips every element.
! Same class of bug as BLOCKING-1: the fix forgot to mirror the
! sign-of-step pattern from the regular DO lowering.
!
! XFAIL: audit BLOCKING-2 (slice print negative stride exits immediately)
! CHECK: 50 40 30 20 10
program audit4_b2_slice_print_negative_stride
  integer :: a(5) = [10, 20, 30, 40, 50]
  print *, a(5:1:-1)
end program
