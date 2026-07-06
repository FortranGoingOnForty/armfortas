! Regression (audit C2): a derived-type VALUE dummy must be rejected, not
! silently miscompiled. The by-value aggregate ABI is unwired, so the
! callee read the dummy's components as constant 0 (target-independent —
! the by-pointer IR is shared across x86_64 and arm64). Reject loudly
! until the calling convention lands. c_ptr/c_funptr are exempt (scalar
! pointers) — see bind_c_cptr_value_and_result_ok.f90.
!
! ERROR_EXPECTED: VALUE attribute on derived-type dummies is not supported
module bcdtv
  use iso_c_binding
  implicit none
  type, bind(C) :: pt
    integer(c_int) :: x, y
  end type
contains
  subroutine takes(s) bind(C, name="afs_c2_takes")
    type(pt), value :: s
    print '(i0,1x,i0)', s%x, s%y
  end subroutine
end module
