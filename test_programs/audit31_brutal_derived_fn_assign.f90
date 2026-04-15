! audit31 Finding 5: `c = add_t(a, b)` SIGSEGV'd because the
! Function lowering arm only checked the HEADER's explicit return
! type to decide whether the result variable was derived-type. A
! function written as `function add_t(...) result(r); type(t) :: r`
! has no header return type — the info is in the body's declaration.
! New helper derived_type_name_for_result_var looks at both, so
! the result variable now allocates a struct buffer and registers
! with derived_type = Some(...), and body component assignments
! land on that buffer. Task #486.
! CHECK: 4 6
program audit31_derived_fn_assign
  implicit none
  type :: t
    integer :: x = 0, y = 0
  end type
  type(t) :: a, b, c
  a%x = 1; a%y = 2
  b%x = 3; b%y = 4
  c = add_t(a, b)           ! <-- SIGSEGV here
  print *, c%x, c%y
contains
  function add_t(a, b) result(r)
    type(t), intent(in) :: a, b
    type(t) :: r
    r%x = a%x + b%x
    r%y = a%y + b%y
  end function
end program
