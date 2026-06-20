! Regression: reading a component of a USE-renamed derived type must
! resolve the underlying layout's fields. jonquil renames `json_error
! => toml_error`; a `type(json_error)` local recorded the alias in its
! LocalInfo, so `j_error%message` looked up a layout named "json_error"
! (none) and warned "no field 'message'", breaking the following
! MOVE_ALLOC. resolve_component_base now canonicalizes the renamed name
! to its layout name. The generic specific fills the object through the
! canonical-named dummy; the caller reads back through the alias.
! Surfaced building fpm: jonquil json_load + j_error%message. x12.
!
! CHECK: x=5
module base
  implicit none
  type :: toml_value
    integer :: x
  end type
  interface fill
    module procedure fill_impl
  end interface
contains
  subroutine fill_impl(o, name)
    class(toml_value), allocatable, intent(out) :: o
    character(*), intent(in) :: name
    allocate(o)
    o%x = len(name)
  end subroutine
end module
module wrap
  use base, only: fill, json_value => toml_value
  implicit none
contains
  subroutine doit()
    class(json_value), allocatable :: jv
    call fill(jv, 'hello')
    write(*, '(a,i0)') 'x=', jv%x
  end subroutine
end module
program p
  use wrap
  call doit()
end program p
