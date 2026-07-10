program ar12_inquiry_of_expr
  implicit none
  integer :: e(3)
  integer :: a(2,3)

  e = [1, 2, 3]
  a = reshape([1, 2, 3, 4, 5, 6], shape(a))

  print '(i0)', size([1, 2, 3])
  ! CHECK: 3
  print '(i0)', size(e + 1)
  ! CHECK: 3
  print '(i0)', size(maxloc(e))
  ! CHECK: 1
  print '(i0)', size(minloc(e))
  ! CHECK: 1
  print '(i0)', size(maxloc(a))
  ! CHECK: 2
end program
