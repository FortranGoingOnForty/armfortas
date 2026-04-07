! Whole-array print: `print *, arr`. Used to mis-dispatch through
! the IrType::Ptr arm of lower_write_items_adv and call
! afs_write_string with a bogus length, producing empty output.
! Audit CRITICAL-2.
!
! CHECK: 11 22 33
! CHECK: 100 200 300 400
program whole_array_print
  integer :: a(3) = [11, 22, 33]
  integer :: b(4)

  print *, a

  b(1) = 100
  b(2) = 200
  b(3) = 300
  b(4) = 400
  print *, b
end program
