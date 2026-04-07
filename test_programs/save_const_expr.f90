! SAVE with a const-expression initializer. Before the fix,
! `integer :: x = 1 + 2` fell through eval_const_global_init to
! the alloca+per-call-store path because the folder only handled
! single literals. Counter would reset to 3 every call.
! Audit CRITICAL-1.
!
! CHECK: 4
! CHECK: 5
! CHECK: 6
program save_const_expr
  call s()
  call s()
  call s()
contains
  subroutine s()
    integer :: x = 1 + 2
    x = x + 1
    print *, x
  end subroutine
end program
