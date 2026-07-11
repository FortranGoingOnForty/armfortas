! Test FMADD/FMSUB/FNMSUB peephole fusion (-Ofast only).
! These exactly representable cases produce the same output at every level.
!
! The arithmetic uses subroutine arguments so constant folding cannot
! eliminate the fmul+fadd/fsub pairs. This forces the peephole to
! actually fire at -Ofast while strict levels keep separate operations.
program fma_peephole
  implicit none
  call test_fma_f32(2.0, 3.0, 4.0)
  call test_fma_f64(1.5d0, 2.0d0, 0.5d0)
end program fma_peephole

subroutine test_fma_f32(a, b, c)
  implicit none
  real, intent(in) :: a, b, c
  real :: r

  ! a*b + c = 10.0  (FMADD candidate)
  r = a * b + c
  print *, r

  ! c + a*b = 10.0  (commuted FMADD candidate)
  r = c + a * b
  print *, r

  ! c - a*b = -2.0  (FMSUB candidate)
  r = c - a * b
  print *, r

  ! a*b - c = 2.0   (FNMSUB candidate)
  r = a * b - c
  print *, r
end subroutine test_fma_f32

subroutine test_fma_f64(da, db, dc)
  implicit none
  double precision, intent(in) :: da, db, dc
  double precision :: dr

  ! da*db + dc = 3.5  (FMADD candidate, double)
  dr = da * db + dc
  print *, dr

  ! dc - da*db = -2.5  (FMSUB candidate, double)
  dr = dc - da * db
  print *, dr
end subroutine test_fma_f64
! CHECK: 1.0000000E1
! CHECK: 1.0000000E1
! CHECK: -2.0000000E0
! CHECK: 2.0000000E0
! CHECK: 3.5
! CHECK: -2.5
