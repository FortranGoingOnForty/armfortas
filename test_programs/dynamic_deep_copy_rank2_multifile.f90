! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! MULTIFILE_LINK: dynamic_copy_parent.f90 dynamic_copy_child.f90 dynamic_copy_main.f90
! CHECK: rank2-strided=2 2 88

!--- file: dynamic_copy_parent.f90
module dynamic_copy_parent_m
  implicit none

  type :: base_t
    integer :: marker = 0
  end type base_t

contains

  subroutine assign_rank_two(destination, source)
    class(base_t), allocatable, intent(out) :: destination(:, :)
    class(base_t), intent(in) :: source(:, :)
    destination = source
  end subroutine assign_rank_two

end module dynamic_copy_parent_m

!--- file: dynamic_copy_child.f90
module dynamic_copy_child_m
  use dynamic_copy_parent_m, only: base_t
  implicit none

  type, extends(base_t) :: child_t
    integer, allocatable :: payload(:)
  end type child_t

contains

  subroutine make_source(values)
    class(base_t), allocatable, intent(out) :: values(:, :)
    integer :: i, j

    allocate(child_t :: values(3, 3))
    select type (values)
    type is (child_t)
      do j = 1, 3
        do i = 1, 3
          allocate(values(i, j)%payload(1))
          values(i, j)%payload(1) = 10 * i + j
        end do
      end do
    end select
  end subroutine make_source

end module dynamic_copy_child_m

!--- file: dynamic_copy_main.f90
program dynamic_copy_main
  use dynamic_copy_parent_m
  use dynamic_copy_child_m
  implicit none

  class(base_t), allocatable :: source(:, :), destination(:, :)
  integer :: i, j, payload_sum

  call make_source(source)
  call assign_rank_two(destination, source(1:3:2, 1:3:2))

  select type (source)
  type is (child_t)
    do j = 1, 3
      do i = 1, 3
        source(i, j)%payload(1) = -1
      end do
    end do
  class default
    error stop 1
  end select
  deallocate(source)

  payload_sum = 0
  select type (destination)
  type is (child_t)
    if (size(destination, 1) /= 2) error stop 2
    if (size(destination, 2) /= 2) error stop 3
    do j = 1, 2
      do i = 1, 2
        if (.not. allocated(destination(i, j)%payload)) error stop 4
        payload_sum = payload_sum + destination(i, j)%payload(1)
      end do
    end do
  class default
    error stop 5
  end select

  print '(a,i0,1x,i0,1x,i0)', 'rank2-strided=', &
    size(destination, 1), size(destination, 2), payload_sum
  if (payload_sum /= 88) error stop 6

  deallocate(destination)
end program dynamic_copy_main
