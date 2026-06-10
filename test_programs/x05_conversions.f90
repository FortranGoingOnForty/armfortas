! Sprint x05 curated program: int <-> real conversions both ways and
! across widths (cvtsi2sd / cvttsd2si ladders).
! CHECK: 3
! CHECK: 3.750000
! CHECK: -2
program x05_conversions
  implicit none
  integer :: i, t
  real(8) :: r
  real(4) :: s
  i = 3
  r = real(i, 8) + 0.75d0
  s = real(r, 4)
  t = int(-2.9d0)
  print *, int(s)
  write (*, '(F8.6)') r
  print *, t
end program x05_conversions
