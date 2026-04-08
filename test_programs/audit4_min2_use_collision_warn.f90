! Audit #4 MINOR-2 — bare `use a; use b` collision warning.
!
! Two modules each declaring an identically-named variable
! brought in via bare USE (no rename, no only-list). The
! install_globals_as_locals helper detects the collision,
! emits a warning to stderr, and keeps the first-imported
! value (deterministic by USE statement order).
!
! This test pins the *runtime* contract: program output is the
! value from the first USE'd module. The warning fires to
! stderr and is not checked here (the harness can't yet
! grep stderr for substrings without ERROR_EXPECTED, which
! requires compilation failure). A future harness extension
! could add `STDERR_EXPECTED` for non-fatal diagnostics; for
! now this program prevents the collision-resolution behavior
! from regressing silently.
!
! CHECK: 1
program audit4_min2_use_collision_warn
  use audit4_min2_a
  use audit4_min2_b
  print *, x
end program

module audit4_min2_a
  integer :: x = 1
end module audit4_min2_a

module audit4_min2_b
  integer :: x = 2
end module audit4_min2_b
