! l01: BOZ literals across the F2023 context list (7.7 / 10.1.5.2):
! named-constant initializer, intrinsic-assignment RHS, typed array
! constructor, REAL/DBLE bit-pattern transfer (16.9.160 — z'40490FDB'
! is pi as IEEE-754 binary32), kind-selected REAL, enumerator value.
! FLAGS: --std=f2023
! CHECK: 10
! CHECK: 255
! CHECK: 1 7 15
! CHECK: 314
! CHECK: 314
! CHECK: 314
! CHECK: 16 17
program l01_boz_contexts
  implicit none
  integer, parameter :: p = b'1010'
  integer :: v, arr(3)
  real :: r
  real(8) :: d, d2
  enum, bind(c)
    enumerator :: red = z'10', green
  end enum
  print *, p
  v = z'ff'
  print *, v
  arr = [integer :: b'1', o'7', z'f']
  print *, arr
  r = real(z'40490FDB')
  print *, int(r * 100)
  d = dble(z'400921FB54442D18')
  print *, int(d * 100)
  d2 = real(z'400921FB54442D18', 8)
  print *, int(d2 * 100)
  print *, red, green
end program l01_boz_contexts
