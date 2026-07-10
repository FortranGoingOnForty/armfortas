! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! CHECK: assigned=7
! CHECK: after-destination=1 7
! CHECK: after-source=2 14

module rank15_polymorphic_finalization_m
  implicit none

  integer :: final_calls = 0
  integer :: final_sum = 0

  type :: base_t
  end type base_t

  type, extends(base_t) :: child_t
    integer :: value = 7
  contains
    final :: finish_child_rank15
  end type child_t

contains

  subroutine finish_child_rank15(values)
    type(child_t), intent(inout) :: values(:, :, :, :, :, :, :, :, &
      :, :, :, :, :, :, :)

    final_calls = final_calls + 1
    final_sum = final_sum + &
      values(1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1)%value
  end subroutine finish_child_rank15

end module rank15_polymorphic_finalization_m

program rank15_polymorphic_finalization
  use rank15_polymorphic_finalization_m
  implicit none

  class(base_t), allocatable :: source(:, :, :, :, :, :, :, :, &
    :, :, :, :, :, :, :)
  class(base_t), allocatable :: destination(:, :, :, :, :, :, :, :, &
    :, :, :, :, :, :, :)

  allocate(child_t :: source(1, 1, 1, 1, 1, 1, 1, 1, &
    1, 1, 1, 1, 1, 1, 1))
  destination = source

  select type (destination)
  type is (child_t)
    print '(a,i0)', 'assigned=', &
      destination(1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1)%value
    if (destination(1, 1, 1, 1, 1, 1, 1, 1, &
      1, 1, 1, 1, 1, 1, 1)%value /= 7) error stop 1
  class default
    error stop 2
  end select

  deallocate(destination)
  print '(a,i0,1x,i0)', 'after-destination=', final_calls, final_sum
  if (final_calls /= 1 .or. final_sum /= 7) error stop 3

  deallocate(source)
  print '(a,i0,1x,i0)', 'after-source=', final_calls, final_sum
  if (final_calls /= 2 .or. final_sum /= 14) error stop 4
end program rank15_polymorphic_finalization
