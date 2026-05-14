! CHECK: ok
! IR_CHECK: call @afs_modproc_fixture_testdrive_check_logical(
! IR_NOT: call @afs_modproc_fixture_error_check(
! REPRO_CHECK: run
module fixture_error
  implicit none

contains
  subroutine check(condition, msg, code, warn)
    logical, intent(in) :: condition
    character(*), intent(in), optional :: msg
    integer, intent(in), optional :: code
    logical, intent(in), optional :: warn

    if (.not. condition) error stop 1
  end subroutine
end module

module fixture_private_carrier
  use fixture_error, only: check
  implicit none
  private
end module

module fixture_testdrive
  implicit none

  type :: error_type
    integer :: dummy = 0
  end type

  interface check
    module procedure check_logical
  end interface

contains
  subroutine check_logical(error, expression, message)
    type(error_type), allocatable, intent(out) :: error
    logical, intent(in) :: expression
    character(*), intent(in) :: message

    if (.not. expression) allocate(error)
  end subroutine
end module

program stdlib_use_only_generic_check_resolution
  use fixture_testdrive, only: error_type, check
  use fixture_private_carrier
  implicit none
  type(error_type), allocatable :: error

  call check(error, .true., 'ok')
  if (allocated(error)) error stop 2
  print *, 'ok'
end program
