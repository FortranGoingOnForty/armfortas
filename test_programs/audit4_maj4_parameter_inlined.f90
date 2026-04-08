! Audit #4 MAJOR-4 — PARAMETER-attributed locals are now inlined
! at every use site instead of being SAVE-promoted to .data.
!
! Fixed: alloc_decls now detects Attribute::Parameter (or
! standalone PARAMETER stmts) on a scalar local. If the
! initializer const-folds, the value is stored on LocalInfo's
! new `inline_const` field and `Expr::Name` lowering
! materializes the constant directly via b.const_i32/i64/f32/f64
! at every use, with no .data slot allocated for the parameter.
!
! Verified by inspecting --emit-ir output: the IR for `result = k*k`
! shows two `const_int 10` instructions and an `imul`, with NO
! `global_addr @afs_save_*_k` and no `load` of the parameter slot.
!
! CHECK: 100
program audit4_maj4_parameter_inlined
  call s()
contains
  subroutine s()
    integer, parameter :: k = 10
    integer :: result
    result = k * k
    print *, result
  end subroutine
end program
