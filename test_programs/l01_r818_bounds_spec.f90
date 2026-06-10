! l01: F2023 R818 explicit-shape-bounds-spec — all bounds given by
! rank-1 constant arrays, both the upper-only and lower:upper forms.
! Before l01 this was accepted into a corrupt rank-1 descriptor
! (size(a,1) read garbage, size(a,2) read 0).
! FLAGS: --std=f2023
! CHECK: 2 3 2
! CHECK: 21
! CHECK: 0 2 1 4
! CHECK: 48
program l01_r818_bounds_spec
  implicit none
  real :: a([2, 3])
  integer :: b([0, 2]:[1, 4])
  integer :: i, j, total
  do i = 1, 2
    do j = 1, 3
      a(i, j) = real((i - 1) * 3 + j)
    end do
  end do
  print *, size(a, 1), size(a, 2), rank(a)
  print *, int(sum(a))
  total = 0
  do i = 0, 1
    do j = 2, 4
      b(i, j) = i * 10 + j
      total = total + b(i, j)
    end do
  end do
  print *, lbound(b, 1), lbound(b, 2), ubound(b, 1), ubound(b, 2)
  print *, total
end program l01_r818_bounds_spec
