! audit31 Finding 9: `ref%data => pool(slot)(1:length)` used to
! report the wrong entity — the sema pointer-assignment target
! check ran extract_base_name, got `ref` (the struct base), and
! complained that `ref` needed the pointer attribute even though
! the real target is the `data` pointer component. Skip the
! base-name attribute check when the target walks into a
! component (we don't carry per-field attrs in the sema registry
! today). Task #490.
! CHECK: ok
module audit31_ptr_substr
  implicit none
  type :: str_ref
    character(len=:), pointer :: data => null()
  end type
  character(len=64), target :: pool(4)
contains
  subroutine intern(ref, slot, length)
    type(str_ref), intent(inout) :: ref
    integer, intent(in) :: slot, length
    ref%data => pool(slot)(1:length)
  end subroutine
end module

program audit31_ptr_substr_driver
  use audit31_ptr_substr
  implicit none
  print *, 'ok'
end program
