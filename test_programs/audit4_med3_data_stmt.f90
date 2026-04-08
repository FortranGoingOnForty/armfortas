! Audit #4 MEDIUM-3 — DATA statement now lowers correctly.
!
! Fixed: init_decls gained a Decl::DataStmt arm that walks
! each set's objects + values pairwise and emits a store per
! scalar Name target. Implied-do object lists and value-side
! repetition (`5*v`) are noted as future work.
!
! CHECK: 43
program audit4_med3_data_stmt
  integer :: x
  data x /42/
  x = x + 1
  print *, x
end program
