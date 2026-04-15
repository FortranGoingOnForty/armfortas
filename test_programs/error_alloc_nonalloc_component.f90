! ALLOCATE of a non-allocatable, non-pointer component.  Previously
! the sema short-circuited to the base variable's attribute check;
! now that FieldLayout carries per-field attributes the validator
! walks the component chain and reports the leaf field.
! ERROR_EXPECTED: only allocatable or pointer components can appear in ALLOCATE
program t
  implicit none
  type :: bundle_t
    integer, allocatable :: good(:)
    integer :: bad(4)          ! fixed-size, not allocatable
  end type
  type(bundle_t) :: obj
  allocate(obj%bad(10))
end program
