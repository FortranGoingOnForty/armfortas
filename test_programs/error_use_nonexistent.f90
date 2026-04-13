! USE of non-existent module should produce a diagnostic.
! ERROR_EXPECTED: not found
program t
  use nonexistent_module_xyz
  implicit none
end program
