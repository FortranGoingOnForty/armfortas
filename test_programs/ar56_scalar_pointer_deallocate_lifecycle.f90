! DEALLOCATE of a scalar derived pointer must finalize its associated target
! and recursively release owned components before the raw allocation is freed.
! An unassociated pointer must reach the runtime error path without lifecycle
! work dereferencing its null target.
!
! CHECK: events=123456
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: pointer_dealloc_cleanup
! IR_CHECK: call @afs_deallocate_pointer
! IR_CHECK: call @afs_modproc_ar56_scalar_pointer_deallocate_lifecycle_m_finish_owner
! IR_CHECK: call @afs_modproc_ar56_scalar_pointer_deallocate_lifecycle_m_finish_child
module ar56_scalar_pointer_deallocate_lifecycle_m
  implicit none

  integer :: events = 0

  type :: child_t
    integer :: marker = 0
  contains
    final :: finish_child
  end type child_t

  type :: owner_t
    integer :: marker = 0
    type(child_t), allocatable :: child
  contains
    final :: finish_owner
  end type owner_t

  type :: holder_t
    type(owner_t), pointer :: item => null()
  end type holder_t

contains

  subroutine finish_owner(owner)
    type(owner_t), intent(inout) :: owner
    if (.not. allocated(owner%child)) error stop 91
    events = events * 10 + owner%marker
  end subroutine finish_owner

  subroutine finish_child(child)
    type(child_t), intent(inout) :: child
    events = events * 10 + child%marker
  end subroutine finish_child

  subroutine release_owner(owner, stat)
    type(owner_t), pointer, intent(inout) :: owner
    integer, intent(out) :: stat
    deallocate(owner, stat=stat)
  end subroutine release_owner

end module ar56_scalar_pointer_deallocate_lifecycle_m

program ar56_scalar_pointer_deallocate_lifecycle
  use ar56_scalar_pointer_deallocate_lifecycle_m
  implicit none

  type(owner_t), pointer :: direct => null()
  type(holder_t) :: holder
  integer :: stat

  allocate(direct)
  direct%marker = 1
  allocate(direct%child)
  direct%child%marker = 2
  deallocate(direct, stat=stat)
  if (stat /= 0 .or. associated(direct)) error stop 1
  if (events /= 12) error stop 2

  allocate(holder%item)
  holder%item%marker = 3
  allocate(holder%item%child)
  holder%item%child%marker = 4
  deallocate(holder%item)
  if (associated(holder%item)) error stop 3
  if (events /= 1234) error stop 4

  allocate(direct)
  direct%marker = 5
  allocate(direct%child)
  direct%child%marker = 6
  call release_owner(direct, stat)
  if (stat /= 0 .or. associated(direct)) error stop 5
  if (events /= 123456) error stop 6

  deallocate(direct, stat=stat)
  if (stat == 0) error stop 7
  if (events /= 123456) error stop 8

  print '(a,i0)', 'events=', events
end program ar56_scalar_pointer_deallocate_lifecycle
