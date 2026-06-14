! l02a item 7 (F2023 C1525): a .NIL. conditional-argument arm selects the
! absent association and is legal only against an OPTIONAL dummy. Here the
! dummy x is required, so passing .NIL. must be rejected loudly (the callee
! would otherwise dereference a null). The valid OPTIONAL case runs in
! l02_conditional_args.f90.
! FLAGS: --std=f2023
! ERROR_EXPECTED: requires an OPTIONAL dummy
program l02a_nil_required_reject
  implicit none
  integer :: a
  a = 3
  call req((a > 0 ? a : .nil.))
contains
  subroutine req(x)
    integer, intent(in) :: x
    print *, x
  end subroutine req
end program l02a_nil_required_reject
