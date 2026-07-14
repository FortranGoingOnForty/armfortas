! l07: a SUBMODULE whose ancestor module is not available is rejected
! (F2008 C1113) rather than silently producing a dangling unit.
! FLAGS: --std=f2023
! ERROR_EXPECTED: parent module 'no_such_mod' was not found
submodule (no_such_mod) impl
end submodule
