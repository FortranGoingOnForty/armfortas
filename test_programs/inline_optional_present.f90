! Inlining a function that reads an OPTIONAL parameter via PRESENT()
! previously tripped an IR-verifier failure at O2: when a caller
! omitted the optional argument, the Fortran lowering passed the i64
! null placeholder, and the inliner mapped `load %b` in the clone
! onto a load from that i64 constant.
!
! The fix refuses to inline any call site whose argument type
! mismatches the callee parameter type; OPTIONAL-absent calls keep
! using the placeholder at the call boundary, where PRESENT() guards
! every read.
program inline_optional_present
  implicit none
  call g(5)
  call g(5, 10)
contains
  subroutine g(a, b)
    integer, intent(in) :: a
    integer, intent(in), optional :: b
    if (present(b)) then
      print *, "both:", a, b
    else
      print *, "a only:", a
    end if
  end subroutine g
end program inline_optional_present
! CHECK: a only: 5
! CHECK: both: 5 10
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
