! Runtime-trip partial unrolling: trip count is opaque (derived from
! `command_argument_count()` so the inliner can't fold it). The
! partial-unroll pass emits a head_bound computation in the
! preheader (`((bound - init + 1) / U) * U + init - 1`) and a scalar
! remainder loop after the unrolled main loop. This fixture is run
! with no args, so n = 33; with U = 4, head_count = 32, and the
! remainder runs 1 scalar iteration covering a(33).
!
! sum(1..33) = 33 * 34 / 2 = 561; a(33) = 33.
! CHECK: 33 561 33
program test_loop_partial_unroll_runtime
  implicit none
  integer :: a(64), s, n, i
  n = 33 + command_argument_count()
  do i = 1, n
    a(i) = i
  end do
  s = 0
  do i = 1, n
    s = s + a(i)
  end do
  print *, n, s, a(n)
end program test_loop_partial_unroll_runtime
