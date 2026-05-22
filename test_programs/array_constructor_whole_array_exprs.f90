! Whole-array expressions inside array constructors must flatten with their
! full element count, including module PARAMETER globals. This mirrors
! stdlib_stats var/cov setup: reshape([s, s * 2, s * 4], shape(s3)).
!
! CHECK: ok
! IR_CHECK: global @afs_mod_array_constructor_whole_array_exprs_data_s3: [f32 x 36] = [1
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module array_constructor_whole_array_exprs_data
  implicit none

  real(4), parameter :: s(4, 3) = reshape([1.0_4, 3.0_4, 5.0_4, 7.0_4, &
                                           2.0_4, 4.0_4, 6.0_4, 8.0_4, &
                                           9.0_4, 10.0_4, 11.0_4, 12.0_4], [4, 3])
  real(4), parameter :: s3(4, 3, 3) = reshape([s, s * 2.0_4, s * 4.0_4], shape(s3))
end module array_constructor_whole_array_exprs_data

program array_constructor_whole_array_exprs
  use array_constructor_whole_array_exprs_data, only: s, s3
  implicit none

  real(4) :: got1(3, 3), want1(3, 3)
  real(4) :: got2(4, 3), want2(4, 3)
  real(4) :: got3(4, 3), want3(4, 3)
  real(4) :: mean1(3, 3), acc1(3, 3)
  integer :: i

  got1 = sum(s3, dim=1)
  want1 = reshape([16.0_4, 20.0_4, 42.0_4, &
                   32.0_4, 40.0_4, 84.0_4, &
                   64.0_4, 80.0_4, 168.0_4], [3, 3])
  if (any(abs(got1 - want1) > 1.0e-5_4)) error stop 1

  got2 = sum(s3, dim=2)
  want2 = reshape([12.0_4, 17.0_4, 22.0_4, 27.0_4, &
                   24.0_4, 34.0_4, 44.0_4, 54.0_4, &
                   48.0_4, 68.0_4, 88.0_4, 108.0_4], [4, 3])
  if (any(abs(got2 - want2) > 1.0e-5_4)) error stop 2

  got3 = sum(s3, dim=3)
  want3 = s + s * 2.0_4 + s * 4.0_4
  if (any(abs(got3 - want3) > 1.0e-5_4)) error stop 3

  mean1 = got1 / real(size(s3, 1), 4)
  acc1 = 0.0_4
  do i = 1, size(s3, 1)
    acc1 = acc1 + (s3(i, :, :) - mean1)**2
  end do
  acc1 = acc1 / real(size(s3, 1) - 1, 4)
  want1 = reshape([20.0_4 / 3.0_4, 20.0_4 / 3.0_4, 5.0_4 / 3.0_4, &
                   4.0_4 * 20.0_4 / 3.0_4, 4.0_4 * 20.0_4 / 3.0_4, &
                   4.0_4 * 5.0_4 / 3.0_4, &
                   16.0_4 * 20.0_4 / 3.0_4, 16.0_4 * 20.0_4 / 3.0_4, &
                   16.0_4 * 5.0_4 / 3.0_4], [3, 3])
  if (any(abs(acc1 - want1) > 1.0e-4_4)) error stop 4

  print *, 'ok'
end program array_constructor_whole_array_exprs
