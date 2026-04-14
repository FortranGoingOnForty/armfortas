! Undeclared FUNCTION calls must also be rejected under IMPLICIT
! NONE — not only bare variable references. Previously validate
! skipped the callee of a FunctionCall, so `foo(3)` silently
! produced a link-time error against an unresolved `_foo`.
! ERROR_EXPECTED: not declared
program t
  implicit none
  print *, foo(3)
end program
