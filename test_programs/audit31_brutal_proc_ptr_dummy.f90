! XFAIL: proc-ptr from dummy rejected by sema — Task #489
! CHECK: 1
! audit31: procedure-pointer assignment from a dummy procedure rejected.
! Fortran 2003 allows a dummy procedure to be the RHS of a proc-pointer '=>'.
! fortsh/execution/trap_dispatch.f90 depends on this.
! Expected: compile cleanly.
! Actual:   "error: pointer assignment source 'proc' must have target or pointer attribute"
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
