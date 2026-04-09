! Audit #5 MAJOR-3 — init_decls used to emit a dead store into
! the sentinel alloca that backs an inlined PARAMETER.
!
! Background: audit4 MAJOR-4 made scalar PARAMETER locals fold
! at decl time and store the value on LocalInfo.inline_const,
! with a one-byte sentinel alloca kept around so any code path
! that touches `info.addr` still has a valid SSA value. The
! sentinel is never loaded — every Expr::Name use materializes
! the inline constant via b.const_*.
!
! The bug: init_decls didn't know about inline_const, so it
! happily walked the parameter entity, lowered the init expr a
! second time, and emitted `store %k_const, %sentinel`. mem2reg
! would clean it up at -O1+, but at -O0 the dead store sat in
! the IR forever, and the second const-fold ran on every entry
! to the function.
!
! Fixed: init_decls (TypeDecl + ParameterStmt arms) now skips
! locals whose info.inline_const is Some — those are exactly
! the parameters that were folded at decl time and don't need
! a runtime store.
!
! At runtime the program is unchanged (the inlined uses always
! produced the right value), so this is purely an IR-cleanup
! finding. The CHECK line below pins the runtime contract.
!
! CHECK: 100
program audit5_m3_param_dead_store
  call s()
contains
  subroutine s()
    integer, parameter :: k = 10
    integer :: result
    result = k * k
    print *, result
  end subroutine
end program
