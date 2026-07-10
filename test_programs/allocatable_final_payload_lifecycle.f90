! CHECK: final=37
! CHECK: final=91
! CHECK: done
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module allocatable_final_payload_m
  implicit none

  integer :: final_count = 0
  integer :: final_values(2) = 0

  type :: item
    integer :: value = -1
  contains
    final :: finish_item
  end type item
contains
  subroutine finish_item(self)
    type(item), intent(inout) :: self

    final_count = final_count + 1
    if (final_count <= size(final_values)) final_values(final_count) = self%value
    print '(a,i0)', 'final=', self%value
  end subroutine finish_item

  subroutine leave_allocated
    type(item), allocatable :: value

    allocate(value)
    value%value = 37
  end subroutine leave_allocated

  subroutine deallocate_explicitly
    type(item), allocatable :: value

    allocate(value)
    value%value = 91
    deallocate(value)
  end subroutine deallocate_explicitly
end module allocatable_final_payload_m

program allocatable_final_payload_lifecycle
  use allocatable_final_payload_m
  implicit none

  call leave_allocated
  call deallocate_explicitly

  if (final_count /= 2) error stop 1
  if (any(final_values /= [37, 91])) error stop 2
  print '(a)', 'done'
end program allocatable_final_payload_lifecycle
