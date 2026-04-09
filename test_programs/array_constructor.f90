! Array constructors `[1, 2, 3]`. Used to be silently dropped by
! the lowerer because lower_expr_full had no Expr::ArrayConstructor
! arm — programs that initialized arrays from literals quietly got
! uninitialized stack memory.
! Audit CRITICAL-3.
!
! CHECK: 10
! CHECK: 20
! CHECK: 30
! CHECK: 1
! CHECK: 2
! CHECK: 3
! CHECK: 4
program array_constructor
  integer :: a(3)
  integer :: b(4) = [1, 2, 3, 4]

  ! Assignment form: a = [v0, v1, v2]
  a = [10, 20, 30]
  print *, a(1)
  print *, a(2)
  print *, a(3)

  ! Initializer form: integer :: b(4) = [...]
  print *, b(1)
  print *, b(2)
  print *, b(3)
  print *, b(4)
end program
