! CHECK: ok
! IR_CHECK: call @afs_allocate_like_with_elem_size
! IR_CHECK: array_merge_check
! REPRO_CHECK: run
module merge_scalar_sources_array_mask_m
  implicit none
contains
  function eye(n) result(res)
    integer, intent(in) :: n
    real(8) :: res(n, n)
    integer :: i

    res = 0.0_8
    do i = 1, n
      res(i, i) = 1.0_8
    end do
  end function eye
end module merge_scalar_sources_array_mask_m

program merge_scalar_sources_array_mask
  use merge_scalar_sources_array_mask_m
  implicit none

  logical, allocatable :: mask(:, :)
  integer :: i

  mask = merge(.true., .false., eye(3) == 1.0_8)

  do i = 1, 3
    if (.not. mask(i, i)) error stop 1
  end do
  if (mask(1, 2)) error stop 2
  if (mask(2, 1)) error stop 3

  print *, "ok"
end program merge_scalar_sources_array_mask
