! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! MULTIFILE_LINK: final_owned_components_mod.f90 final_owned_components_main.f90
! CHECK: events=13482

!--- file: final_owned_components_mod.f90
module final_owned_components_m
  implicit none

  integer :: events = 0

  type :: child_t
    integer :: value = 0
  contains
    final :: finish_child
  end type child_t

  type :: plain_owner_t
    type(child_t), allocatable :: child
  end type plain_owner_t

  type :: final_owner_t
    type(child_t), allocatable :: child
  contains
    final :: finish_owner
  end type final_owner_t

  type :: nested_owner_t
    type(plain_owner_t) :: owner
  end type nested_owner_t

  type :: array_child_t
    integer :: value = 0
  contains
    final :: finish_child_array
  end type array_child_t

  type :: array_owner_t
    type(array_child_t), allocatable :: children(:)
  end type array_owner_t

contains

  subroutine finish_child(child)
    type(child_t), intent(inout) :: child
    if (child%value == 7) error stop 7
    events = events * 10 + child%value
  end subroutine finish_child

  subroutine finish_owner(owner)
    type(final_owner_t), intent(inout) :: owner
    if (allocated(owner%child)) then
      events = events * 10 + 8
    else
      events = events * 10 + 9
    end if
  end subroutine finish_owner

  subroutine finish_child_array(children)
    type(array_child_t), intent(inout) :: children(:)
    events = events * 10 + size(children)
  end subroutine finish_child_array

  subroutine release_plain_owner()
    type(plain_owner_t) :: owner
    allocate(owner%child)
    owner%child%value = 1
  end subroutine release_plain_owner

  subroutine release_final_owner()
    type(final_owner_t), allocatable :: owner
    allocate(owner)
    allocate(owner%child)
    owner%child%value = 2
    deallocate(owner)
  end subroutine release_final_owner

  subroutine release_nested_owner()
    type(nested_owner_t) :: nested
    allocate(nested%owner%child)
    nested%owner%child%value = 3
  end subroutine release_nested_owner

  subroutine release_array_owner()
    type(array_owner_t) :: owner
    allocate(owner%children(4))
  end subroutine release_array_owner

end module final_owned_components_m

!--- file: final_owned_components_main.f90
program final_owned_components
  use final_owned_components_m
  implicit none
  type(plain_owner_t) :: main_owner

  allocate(main_owner%child)
  main_owner%child%value = 7

  call release_plain_owner()
  call release_nested_owner()
  call release_array_owner()
  call release_final_owner()

  print '(a,i0)', 'events=', events
  if (events /= 13482) error stop 1
end program final_owned_components
