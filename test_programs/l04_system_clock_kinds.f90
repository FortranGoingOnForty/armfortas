! l04: F2023 SYSTEM_CLOCK with conforming integer arguments — all the
! integer arguments share the default kind, so the call is accepted and
! runs. (The rejection cases live in the imported gfortran-dg
! system_clock_4 fixture under --std=f2023.)
!
! Only COUNT_RATE is asserted: with default (kind-4) integers the
! runtime's nanosecond COUNT and i64 COUNT_MAX overflow the 32-bit
! slot (pre-existing, see noted_items.md). COUNT_RATE (1e9) fits.
! FLAGS: --std=f2023
! CHECK: rate T
! CHECK: ok
program l04_system_clock_kinds
  implicit none
  integer :: c, r, m
  c = 0
  r = 0
  m = 0
  call system_clock(count=c, count_rate=r, count_max=m)
  print '(A,1X,L1)', 'rate', r > 0
  print '(A)', 'ok'
end program l04_system_clock_kinds
