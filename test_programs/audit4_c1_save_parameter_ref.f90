! Audit #4 CRITICAL-1 — SAVE silently lost when initializer
! references a named PARAMETER.
!
! eval_const_scalar handles literals + binary arithmetic but has
! no Expr::Name case. `integer :: x = k * 2` where k is a parameter
! falls through to the alloca+per-call-store path because the
! folder can't resolve `k`. The local then re-initializes on every
! call, defeating SAVE.
!
! The fix needs eval_const_scalar to look up parameter names in a
! const-table threaded through LowerCtx (which would also fix
! MAJOR-4: parameter-attributed locals are currently SAVE-promoted
! to .data instead of being inlined at use sites).
!
! Three calls of `s` should print 21, 22, 23.
!
! XFAIL: audit CRITICAL-1 (SAVE init referencing parameter loses semantics)
! CHECK: 21
! CHECK: 22
! CHECK: 23
program audit4_c1_save_parameter_ref
  call s()
  call s()
  call s()
contains
  subroutine s()
    integer, parameter :: k = 10
    integer :: x = k * 2
    x = x + 1
    print *, x
  end subroutine
end program
