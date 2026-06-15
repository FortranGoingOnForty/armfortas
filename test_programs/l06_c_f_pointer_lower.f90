! l06: F2023 C_F_POINTER gains the optional LOWER argument, giving the
! Fortran pointer non-default lower bounds. SHAPE and LOWER may be inline
! constructors or runtime integer arrays. Bounds and element addressing
! (column-major) honor LOWER. Uses the scalar `dim` form of lbound/ubound
! (the whole-array form is a separate, unrelated gap).
! FLAGS: --std=f2023
program l06_c_f_pointer_lower
  use, intrinsic :: iso_c_binding
  implicit none
  type(c_ptr) :: x
  integer, target :: a2(12), a3(24)
  integer, pointer :: p2(:, :), p3(:, :, :)
  integer :: sh2(2), lo2(2), i

  do i = 1, 12
    a2(i) = i
  end do
  do i = 1, 24
    a3(i) = i
  end do

  ! Runtime SHAPE/LOWER arrays, 2-D.
  x = c_loc(a2)
  sh2 = [3, 4]
  lo2 = [2, 2]
  call c_f_pointer(x, p2, shape=sh2, lower=lo2)
  print '(I0,1X,I0)', lbound(p2, 1), lbound(p2, 2)
  ! CHECK: 2 2
  print '(I0,1X,I0)', ubound(p2, 1), ubound(p2, 2)
  ! CHECK: 4 5
  print '(I0,1X,I0)', size(p2, 1), size(p2, 2)
  ! CHECK: 3 4
  print '(I0)', p2(2, 2)
  ! CHECK: 1
  print '(I0)', p2(4, 5)
  ! CHECK: 12

  ! Inline-constructor LOWER, 3-D, negative bounds.
  x = c_loc(a3)
  call c_f_pointer(x, p3, [2, 3, 4], [-1, -2, -3])
  print '(I0,1X,I0,1X,I0)', lbound(p3, 1), lbound(p3, 2), lbound(p3, 3)
  ! CHECK: -1 -2 -3
  print '(I0,1X,I0,1X,I0)', ubound(p3, 1), ubound(p3, 2), ubound(p3, 3)
  ! CHECK: 0 0 0
  print '(I0)', p3(-1, -2, -3)
  ! CHECK: 1
  print '(I0)', p3(0, 0, 0)
  ! CHECK: 24

  ! Without LOWER: default lower bound of 1 (regression guard).
  call c_f_pointer(c_loc(a2), p2, [3, 4])
  print '(I0,1X,I0)', lbound(p2, 1), lbound(p2, 2)
  ! CHECK: 1 1
  print '(I0)', p2(3, 4)
  ! CHECK: 12
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l06_c_f_pointer_lower
