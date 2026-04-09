! Test loop interchange optimization.
! A nested loop with column-major-hostile access pattern should
! produce the same results at all optimization levels.
program loop_interchange
  implicit none
  integer :: a(10, 10)
  integer :: i, j

  ! This access pattern is column-major-hostile: inner loop (j) strides
  ! over columns while outer loop (i) strides over rows. At O2+,
  ! interchange should swap the loop order for better cache behavior
  ! while preserving the computed values.
  do i = 1, 10
    do j = 1, 10
      a(i, j) = i * 10 + j
    end do
  end do

  ! Print corners to verify correctness.
  print *, a(1, 1)
  print *, a(10, 1)
  print *, a(1, 10)
  print *, a(10, 10)
end program loop_interchange
! CHECK: 11
! CHECK: 101
! CHECK: 20
! CHECK: 110
