! CHECK: ok
! IR_CHECK: call @afs_deallocate_array
! REPRO_CHECK: run
module intent_out_allocatable_array_reallocate_m
  implicit none

  type :: set_t
    integer :: n = 0
  end type

  interface assignment(=)
    module procedure logical_assign
  end interface
contains
  subroutine logical_assign(lhs, rhs)
    logical(1), allocatable, intent(out) :: lhs(:)
    type(set_t), intent(in) :: rhs

    allocate(lhs(rhs%n))
    lhs = .true.
  end subroutine
end module

program intent_out_allocatable_array_reallocate
  use intent_out_allocatable_array_reallocate_m
  implicit none

  type(set_t) :: set
  logical(1), allocatable :: values(:)

  set%n = 64
  values = set
  if (size(values) /= 64) error stop 1

  set%n = 66
  values = set
  if (size(values) /= 66) error stop 2
  if (.not. all(values)) error stop 3
  print *, "ok"
end program
