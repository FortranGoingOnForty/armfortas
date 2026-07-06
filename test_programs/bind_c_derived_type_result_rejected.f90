! Regression (audit C2): a BIND(C) function returning a derived type must
! be rejected. The aggregate-return ABI is unwired, so the result came
! back from a never-written buffer (read 0) at every struct size and on
! every target. Reject loudly until the C struct-return convention lands.
! c_ptr/c_funptr results are exempt (scalar pointers).
!
! ERROR_EXPECTED: BIND(C) function returning a derived type is not supported
module bcdtr
  use iso_c_binding
  implicit none
  type, bind(C) :: pt
    integer(c_int) :: x, y
  end type
contains
  function make(a, b) result(r) bind(C, name="afs_c2_make")
    integer(c_int), value :: a, b
    type(pt) :: r
    r%x = a
    r%y = b
  end function
end module
