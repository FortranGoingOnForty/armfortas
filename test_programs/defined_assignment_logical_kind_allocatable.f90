! CHECK: ok
! IR_CHECK: call @afs_modproc_defined_assignment_logical_kind_allocatable_m_log2_assign(
! IR_CHECK: call @afs_modproc_defined_assignment_logical_kind_allocatable_m_log8_assign(
! REPRO_CHECK: run
module defined_assignment_logical_kind_allocatable_m
  implicit none

  type :: set_t
    integer :: n = 3
  end type set_t

  interface assignment(=)
    module procedure log2_assign
    module procedure log8_assign
  end interface

contains
  subroutine log2_assign(lhs, rhs)
    logical(2), allocatable, intent(out) :: lhs(:)
    type(set_t), intent(in) :: rhs

    allocate(lhs(rhs%n))
    lhs = .true.
  end subroutine log2_assign

  subroutine log8_assign(lhs, rhs)
    logical(8), allocatable, intent(out) :: lhs(:)
    type(set_t), intent(in) :: rhs

    allocate(lhs(rhs%n + 1))
    lhs = .true.
  end subroutine log8_assign
end module defined_assignment_logical_kind_allocatable_m

program defined_assignment_logical_kind_allocatable
  use defined_assignment_logical_kind_allocatable_m
  implicit none
  type(set_t) :: set
  logical(2), allocatable :: log2(:)
  logical(8), allocatable :: log8(:)

  log2 = set
  log8 = set

  if (size(log2) /= 3) error stop 1
  if (.not. all(log2)) error stop 2
  if (size(log8) /= 4) error stop 3
  if (.not. all(log8)) error stop 4
  print *, 'ok'
end program defined_assignment_logical_kind_allocatable
