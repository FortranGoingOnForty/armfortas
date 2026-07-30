! Scalar reductions must walk every coordinate of rank-N array and mask
! descriptors. The array section reverses both dimensions while the conforming
! mask section uses different positive strides, so flattening either descriptor
! through only dims(1)%stride observes the wrong elements.
!
! FLAGS: --std=f2023
! CHECK: 1015 40 960 1 20
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: afs_array_norm2_real8
! IR_CHECK: afs_array_sum_real8_mask
program ar44_scalar_reduction_strides
  implicit none
  integer :: i, j
  integer :: ints(5, 4)
  real :: real4s(5, 4)
  real(8) :: real8s(5, 4)
  logical :: mask(5, 4)

  do j = 1, 4
    do i = 1, 5
      ints(i, j) = 5 * (j - 1) + i
      real4s(i, j) = real(ints(i, j))
      real8s(i, j) = real(ints(i, j), 8)
    end do
  end do

  mask = .false.
  mask(1, 1) = .true.
  mask(5, 1) = .true.
  mask(3, 4) = .true.
  mask(5, 4) = .true.

  if (abs(norm2(real8s(5:1:-2, 4:1:-3)) ** 2 - 1015.0_8) > 1.0e-10_8) error stop 1
  if (abs(real(norm2(real4s(5:1:-2, 4:1:-3)), 8) ** 2 - 1015.0_8) > 1.0e-3_8) error stop 2

  if (sum(real8s(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 40.0_8) error stop 3
  if (product(real4s(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 960.0) error stop 4
  if (sum(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 40) error stop 5
  if (product(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 960) error stop 6
  if (minval(real8s(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 1.0_8) error stop 7
  if (maxval(real4s(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 20.0) error stop 8
  if (minval(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 1) error stop 9
  if (maxval(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)) /= 20) error stop 10

  if (norm2(real8s(5:1, :)) /= 0.0_8) error stop 11
  if (sum(real8s(5:1, :), mask=mask(5:1, :)) /= 0.0_8) error stop 12
  if (product(ints(5:1, :), mask=mask(5:1, :)) /= 1) error stop 13

  print *, nint(norm2(real8s(5:1:-2, 4:1:-3)) ** 2), &
      sum(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)), &
      product(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)), &
      minval(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3)), &
      maxval(ints(5:1:-2, 4:1:-3), mask=mask(1:5:2, 1:4:3))
end program ar44_scalar_reduction_strides
