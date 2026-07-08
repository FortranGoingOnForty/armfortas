! Audit C8: an io-implied-do in a list-directed / unformatted output list
! (`print *, (a(i), i=1,n)`, `write(*,*) (...)`) was dropped entirely and the
! record printed blank. Two bugs: (1) parse_print used a plain expression loop,
! so the implied-do's `var=` was a parse error; it now shares parse_io_expr_list
! with WRITE. (2) The list-directed lowering (lower_write_items_adv) had no
! array-constructor case, so the item lowered to a stack pointer written as a
! zero-length string; it now walks the constructor / implied-do element-wise,
! mirroring the formatted path (lower_fmt_push_ac_values). The formatted case
! (`print '(...)' , (...)`) already worked and is exercised too.
program implied_do_list_directed_output
  integer :: ia(6) = [1, 2, 3, 4, 5, 6]
  integer :: i, j
  character(3) :: cs(2) = ['abc', 'def']

  ! list-directed implied-do (the audit reproducer)
  print *, (ia(i), i=1,3)
  ! CHECK: 1           2           3
  ! unformatted WRITE implied-do with a stride
  write(*,*) (ia(i), i=1,6,2)
  ! CHECK: 1           3           5
  ! negative stride
  print *, (ia(i), i=6,1,-2)
  ! CHECK: 6           4           2
  ! nested implied-do
  print *, ((i*10 + j, j=1,2), i=1,2)
  ! CHECK: 11          12          21          22
  ! character implied-do
  print *, (cs(i), i=1,2)
  ! CHECK: abc def
  ! plain array constructor in an output list
  write(*,*) [7, 8, 9]
  ! CHECK: 7           8           9
  ! implied-do mixed with scalar items
  print *, 'x', (ia(i), i=1,2), 'y'
  ! CHECK: x           1           2 y
  ! formatted implied-do still works (the path that was already correct)
  print '(3I4)', (ia(i), i=1,3)
  ! CHECK: 1   2   3
end program
