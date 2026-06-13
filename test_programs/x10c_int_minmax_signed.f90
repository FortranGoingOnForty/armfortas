! x10c-1: signed i32 packed min/max synthesized on SSE2 via the
! pcmpgtd compare-and-blend idiom (no native pminsd/pmaxsd until
! SSE4.1). The signedness matters — negatives must order below
! positives — so this exercises a mix of signs across the lanes and
! the reduction tree. OPT_EQ ties the vectorized O2+ output to the
! scalar O0 result across every level — the correctness guarantee for
! the synthesis (matching the do_loop_vectorize_* fixtures, which pin no
! instruction: ASM_CHECK has no opt-level scope and vectorization is
! O2+ only).
! FLAGS: --std=f2023
! CHECK: emax 8
! CHECK: emin -26
! CHECK: rmax 8
! CHECK: rmin -26
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10c_int_minmax_signed
  implicit none
  integer, parameter :: n = 16
  integer :: a(n), b(n), c(n), d(n), i
  integer :: emax, emin, rmax, rmin

  ! Spread of signs and magnitudes across lanes.
  do i = 1, n
    a(i) = mod(i * 7 - 50, 19) - 9     ! ranges over negatives and positives
    b(i) = 8 - mod(i * 5, 17)
  end do

  ! Elementwise signed max/min.
  do i = 1, n
    c(i) = max(a(i), b(i))
    d(i) = min(a(i), b(i))
  end do
  emax = -2000000000
  emin = 2000000000
  do i = 1, n
    emax = max(emax, c(i))   ! overall largest elementwise-max
    emin = min(emin, d(i))   ! overall smallest elementwise-min
  end do

  ! Direct min/max reductions over a (full-range, sentinel seed — the
  ! shape the reduction synthesis recognizes).
  rmax = -2000000000
  rmin = 2000000000
  do i = 1, n
    rmax = max(rmax, a(i))
    rmin = min(rmin, a(i))
  end do

  print '(A,1X,I0)', 'emax', emax
  print '(A,1X,I0)', 'emin', emin
  print '(A,1X,I0)', 'rmax', rmax
  print '(A,1X,I0)', 'rmin', rmin
  print '(A)', 'ok'
end program x10c_int_minmax_signed
