! audit31 Finding 16: F2018 §11.1.4 gives a BLOCK construct its own
! implicit-typing environment.  Outer `implicit none` plus a block-
! scoped `implicit integer (i-n)` should let `n` be implicitly typed
! inside the block.  The Stmt::Block AST dropped the parsed implicit
! decls, the IMPLICIT NONE walker ignored block-local rules, and the
! lowerer never allocated implicitly-typed locals.  Carry an
! `implicit: Vec<SpannedDecl>` field on Stmt::Block, layer the
! covering letters into walk_stmt_for_undeclared, and pre-walk the
! block body in the lowerer to synthesise TypeDecls for any
! implicitly-typed name the body references.  Task #497.
! CHECK: 15
program audit31_implicit_block
  implicit none
  integer :: a
  a = 5
  block
    implicit integer (i-n)
    n = a + 10
    print *, n
  end block
end program
