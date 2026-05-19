! CHECK: ok
! IR_CHECK: call @afs_copy_array_result_to_fixed(
! IR_NOT: call @memcpy(
! REPRO_CHECK: run
module fixture_zero_size_result
contains
  function make(flag) result(r)
    logical, intent(in) :: flag
    real, allocatable :: r(:,:)

    if (flag) then
      allocate(r(0, 0))
    else
      allocate(r(2, 2))
      r = reshape([1.0, 2.0, 3.0, 4.0], [2, 2])
    end if
  end function
end module

program stdlib_zero_size_array_result_fixed_dest
  use fixture_zero_size_result
  implicit none
  real :: a(2, 2)

  a = -1.0
  a = make(.true.)
  if (a(1, 1) /= 0.0 .or. a(2, 2) /= 0.0) error stop 1

  a = make(.false.)
  if (abs(a(1, 1) - 1.0) > 1.0e-6) error stop 2
  if (abs(a(2, 2) - 4.0) > 1.0e-6) error stop 3

  print *, 'ok'
end program
