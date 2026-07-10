program ar12_shift_intrinsics
  implicit none
  integer :: v(6) = [3, 1, 4, 1, 5, 9]
  integer :: a(5)
  integer :: r6(6)
  integer :: r5(5)
  character(2) :: c(4)
  character(2) :: rc(4)

  a = [1, 2, 3, 4, 5]

  r6 = cshift(v, 2)
  print '(6i1)', r6
  ! CHECK: 415931

  r6 = eoshift(v, -1)
  print '(6i1)', r6
  ! CHECK: 031415

  r5 = cshift(array=a, shift=-1, dim=1)
  print '(5i1)', r5
  ! CHECK: 51234

  r5 = eoshift(a, -2, boundary=7)
  print '(5i1)', r5
  ! CHECK: 77123

  c = ['aa', 'bb', 'cc', 'dd']

  rc = cshift(c, 1)
  print '(4a2)', rc
  ! CHECK: bbccddaa

  rc = eoshift(array=c, shift=1, boundary='zz')
  print '(4a2)', rc
  ! CHECK: bbccddzz
end program ar12_shift_intrinsics
