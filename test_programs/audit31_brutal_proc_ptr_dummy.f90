! audit31 Finding 8: `cb => proc` where proc is a `procedure(iface)`
! dummy argument used to fail with "pointer assignment source
! must have target or pointer attribute". F2003 allows a dummy
! procedure as the RHS of a procedure-pointer '=>'; relax the
! sema check to accept symbols carrying attrs.external (which is
! how `procedure(iface) :: proc` is lowered) in addition to the
! Function/Subroutine kinds. Task #489.
! CHECK: ok
module audit31_procptr_dummy
  implicit none
  abstract interface
    subroutine iface(x)
      integer, intent(in) :: x
    end subroutine
  end interface
  procedure(iface), pointer :: cb => null()
contains
  subroutine set_cb(proc)
    procedure(iface) :: proc
    cb => proc
  end subroutine
end module

program audit31_procptr_dummy_driver
  use audit31_procptr_dummy
  implicit none
  print *, 'ok'
end program
