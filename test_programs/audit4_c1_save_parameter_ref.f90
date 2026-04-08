! Audit #4 CRITICAL-1 — SAVE init that references a named
! PARAMETER now folds correctly and preserves SAVE semantics.
!
! Fixed: eval_const_scalar takes a `param_consts` map and
! resolves Expr::Name references against it. alloc_decls (and
! collect_module_globals) build the map by walking the decls
! list incrementally so a later parameter declaration can
! reference earlier ones (`tau = 2 * pi`).
!
! `integer :: x = k * 2` (k a parameter) now folds to 20,
! SAVE-promotes to .data, and the counter advances correctly
! across calls instead of resetting to 21 every time.
!
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
