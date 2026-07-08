! audit31 harvest: using a derived-type function result INLINE — i.e.
! `add_t(a, b)%x` in a PRINT statement rather than first assigning
! the call to a variable — printed zero components because
! Expr::ComponentAccess's base resolver didn't know how to lower a
! FunctionCall base. resolve_component_base handled only Name and
! ComponentAccess, so the fallback const_i32(0) kicked in. Add a
! `callee_return_derived_type_name` helper and teach the
! ComponentAccess arm in lower_expr_full to lower the call and use
! its result pointer as the component base when the callee returns a
! derived type. Task #481.
! CHECK: assigned: 4 6
! CHECK: inline  :           4           6
program audit31_derived_fn_inline
  implicit none
  type :: t
    integer :: x = 0, y = 0
  end type
  type(t) :: a, b, c
  a%x = 1; a%y = 2
  b%x = 3; b%y = 4
  c = add_t(a, b)
  print *, 'assigned:', c%x, c%y
  print *, 'inline  :', add_t(a, b)%x, add_t(a, b)%y
contains
  function add_t(a, b) result(r)
    type(t), intent(in) :: a, b
    type(t) :: r
    r%x = a%x + b%x
    r%y = a%y + b%y
  end function
end program
