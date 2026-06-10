! l02: the IR shape — branch into arm blocks that merge through a
! block parameter; no speculative evaluation of both arms in the
! entry block.
! FLAGS: --std=f2023
! CHECK: 9
! IR_CHECK: cond_then
! IR_CHECK: cond_else
! IR_CHECK: cond_merge
program l02_conditional_ir_shape
  implicit none
  integer :: n
  n = 4
  print *, (n > 3 ? n + 5 : n - 5)
end program l02_conditional_ir_shape
