! l04/L-tail: SYSTEM_CLOCK keys its resolution off the argument kind
! (gfortran-compatible; F2018 16.9.202 leaves it processor-defined).
! Default (kind-4) integers get a millisecond clock that FITS the
! kind: rate 1000, COUNT_MAX = HUGE(int32), COUNT in [0, COUNT_MAX].
! Previously the runtime wrote nanoseconds and i64::MAX, which
! truncated — COUNT_MAX read back as -1 (noted_items, resolved).
! FLAGS: --std=f2023
! CHECK: rate 1000
! CHECK: max 2147483647
! CHECK: count_ok T
! CHECK: ticks T
! CHECK: ok
program l04_system_clock_kinds
  implicit none
  integer :: c, r, m, c2
  integer(kind=8) :: c8, r8, m8
  c = 0
  r = 0
  m = 0
  call system_clock(count=c, count_rate=r, count_max=m)
  print '(A,1X,I0)', 'rate', r
  print '(A,1X,I0)', 'max', m
  print '(A,1X,L1)', 'count_ok', c >= 0 .and. c <= m
  call system_clock(count=c2)
  ! Monotone modulo the (31-bit millisecond) wrap; both reads within
  ! the same run are far from the wrap boundary in practice.
  print '(A,1X,L1)', 'ticks', c2 >= c .or. c2 < 1000

  ! kind-8 arguments keep the nanosecond clock.
  c8 = 0
  r8 = 0
  m8 = 0
  call system_clock(count=c8, count_rate=r8, count_max=m8)
  if (r8 /= 1000000000_8) print '(A)', 'BAD RATE8'
  if (m8 /= huge(0_8)) print '(A)', 'BAD MAX8'
  if (c8 <= 0_8) print '(A)', 'BAD COUNT8'
  print '(A)', 'ok'
end program l04_system_clock_kinds
