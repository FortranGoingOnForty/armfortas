! Regression (audit C2 exemption): type(c_ptr)/type(c_funptr) are
! interoperable opaque pointer types — ABI-scalar (one GP register), not
! aggregates. A BIND(C) function returning c_ptr and a BIND(C) c_ptr
! VALUE dummy must keep working; the derived-type by-value/return
! rejection must not touch them. Exercises both directions: a c_ptr
! returned across the boundary, then passed by value and dereferenced.
!
! CHECK: 42
module cptrok
  use iso_c_binding
  implicit none
contains
  function ptr_of(x) result(p) bind(C, name="afs_c2_ptr_of")
    integer(c_int), target, intent(in) :: x
    type(c_ptr) :: p
    p = c_loc(x)
  end function
  subroutine deref(q) bind(C, name="afs_c2_deref")
    type(c_ptr), value :: q
    integer(c_int), pointer :: pn
    call c_f_pointer(q, pn)
    print '(i0)', pn
  end subroutine
end module

program bind_c_cptr_value_and_result_ok
  use cptrok
  use iso_c_binding
  implicit none
  integer(c_int), target :: n
  type(c_ptr) :: cp
  n = 42
  cp = ptr_of(n)   ! c_ptr result across BIND(C)
  call deref(cp)   ! c_ptr VALUE dummy
end program bind_c_cptr_value_and_result_ok
