! audit31 Finding 1: keyword arguments now reorder to match the
! callee's declared param list (F2003 §12.4.1.2). Previously
! `call sub(b=10, a=20)` bound positionally → a=10, b=20. A
! new reorder_args_by_keyword pass runs just before arg lowering
! at both the Stmt::Call and Expr::FunctionCall sites. Task #482.
! CHECK: a= 20 b= 10
program audit31_keyword_args
  implicit none
  call sub(b=10, a=20)
contains
  subroutine sub(a, b)
    integer, intent(in) :: a, b
    print *, 'a=', a, 'b=', b
  end subroutine
end program
