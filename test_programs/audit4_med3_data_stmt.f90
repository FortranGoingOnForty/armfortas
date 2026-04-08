! Audit #4 MEDIUM-3 — DATA statement is silently ignored.
!
! The parser accepts `data x /42/` and the recent M2 fix routes
! it to parse_data_stmt, but lower.rs has no handler for
! Decl::DataStmt — init_decls only walks TypeDecl entities and
! ParameterStmt pairs. The DATA statement vanishes between
! parse and IR; the local stays uninitialized at function entry
! and reads stack garbage on first use.
!
! Per F2018 §8.6.6, DATA initializes its targets like an
! initializer in a type-decl. Implementation: extend init_decls
! to walk Decl::DataStmt and emit a store per (target, value)
! pair, with SAVE-promotion if applicable.
!
! XFAIL: audit MEDIUM-3 (DATA statement silently dropped)
! CHECK: 43
program audit4_med3_data_stmt
  integer :: x
  data x /42/
  x = x + 1
  print *, x
end program
