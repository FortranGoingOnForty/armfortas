! CHECK: ok
! IR_CHECK: alloc_array_source_broadcast_check
! IR_CHECK: call @afs_array_size
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program allocate_array_scalar_source
  implicit none

  type :: box_t
    integer, allocatable :: vals(:)
  end type box_t

  integer, allocatable :: seed(:)
  integer, allocatable :: grid(:, :)
  type(box_t) :: box
  integer :: i, j, n

  n = 8
  allocate(seed(n), source = 123456)
  if (.not. allocated(seed)) error stop 1
  if (size(seed) /= n) error stop 2
  do i = 1, n
    if (seed(i) /= 123456) error stop 3
  end do

  allocate(grid(3, 4), source = -7)
  if (.not. allocated(grid)) error stop 4
  if (size(grid, 1) /= 3) error stop 5
  if (size(grid, 2) /= 4) error stop 6
  do j = 1, 4
    do i = 1, 3
      if (grid(i, j) /= -7) error stop 7
    end do
  end do

  allocate(box%vals(5), source = 42)
  if (.not. allocated(box%vals)) error stop 8
  do i = 1, 5
    if (box%vals(i) /= 42) error stop 9
  end do

  write(*, '(a)') "ok"
end program allocate_array_scalar_source
