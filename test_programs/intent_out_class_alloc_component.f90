! CHECK: ok
! IR_CHECK: call @afs_deallocate_array
! REPRO_CHECK: run
module intent_out_class_alloc_component_m
  implicit none

  type :: box_t
    integer, allocatable :: vals(:)
  contains
    procedure :: reset
  end type
contains
  subroutine reset(self, n)
    class(box_t), intent(out) :: self
    integer, intent(in) :: n
    integer :: stat

    allocate(self%vals(n), stat=stat)
    if (stat /= 0) error stop 10
    self%vals = 7
  end subroutine
end module

program intent_out_class_alloc_component
  use intent_out_class_alloc_component_m
  implicit none

  type(box_t) :: box

  call box%reset(2)
  call box%reset(3)
  if (.not. allocated(box%vals)) error stop 1
  if (size(box%vals) /= 3) error stop 2
  print *, "ok"
end program
